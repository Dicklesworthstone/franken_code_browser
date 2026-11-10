import AppKit
import Foundation

/// Real AppKit publication on temporary, privately named pasteboards. Never
/// touches the user's general clipboard. Execute through the independent
/// strict-RCH native verifier; source review alone is not a passing result.
@MainActor @main struct AtlasReaderSelectionClipboardTests {
    static func main() {
        _ = NSApplication.shared
        copiesLogicalUnicodeWithoutNormalization()
        successfulPublicationReplacesAllPreviousItems()
        oversizedPublicationLeavesPreviousContentsAndOwnership()
        changedOwnershipPreservesTheNewerContents()
        exactBudgetBoundaryPublishesWithoutTruncation()
        budgetCountsUTF8BytesRatherThanCharacters()
        emptyLogicalTextIsAnExplicitSuccessfulCopy()
        print("AtlasReaderSelectionClipboard: 7 native clipboard scenarios passed")
    }

    static func withPasteboard(_ body: (NSPasteboard) -> Void) {
        let pasteboard = NSPasteboard.withUniqueName()
        defer { pasteboard.releaseGlobally() }
        body(pasteboard)
    }

    static func seed(_ text: String, on pasteboard: NSPasteboard) -> Int {
        pasteboard.clearContents()
        precondition(pasteboard.setString(text, forType: .string))
        return pasteboard.changeCount
    }

    static func assertText(_ expected: String, on pasteboard: NSPasteboard) {
        guard let actual = pasteboard.string(forType: .string) else {
            preconditionFailure("missing native logical-text flavor")
        }
        precondition(actual.utf8.elementsEqual(expected.utf8),
                     "native publication changed the exact logical Unicode text")
    }

    static func copiesLogicalUnicodeWithoutNormalization() {
        withPasteboard { pasteboard in
            let expected = "\u{feff}e\u{301} ≠ é · 🦀\r\nאב\t\0尾\rend\n"
            let ownership = seed("previous", on: pasteboard)
            precondition(AtlasReaderSelectionClipboard.publish(expected, to: pasteboard,
                expectedChangeCount: ownership) == .copied)
            assertText(expected, on: pasteboard)
            precondition(pasteboard.pasteboardItems?.count == 1)
            precondition(pasteboard.pasteboardItems?.first?.types.contains(.string) == true)
        }
    }

    static func successfulPublicationReplacesAllPreviousItems() {
        withPasteboard { pasteboard in
            let first = NSPasteboardItem(), second = NSPasteboardItem()
            precondition(first.setString("old first", forType: .string))
            precondition(second.setString("old second", forType: .string))
            pasteboard.clearContents()
            precondition(pasteboard.writeObjects([first, second]))
            let ownership = pasteboard.changeCount
            precondition(pasteboard.pasteboardItems?.count == 2)
            precondition(AtlasReaderSelectionClipboard.publish("new exact selection", to: pasteboard,
                expectedChangeCount: ownership) == .copied)
            assertText("new exact selection", on: pasteboard)
            precondition(pasteboard.pasteboardItems?.count == 1,
                         "selected text was appended to the previous clipboard")
        }
    }

    static func oversizedPublicationLeavesPreviousContentsAndOwnership() {
        withPasteboard { pasteboard in
            let ownership = seed("preserve me", on: pasteboard)
            let oversized = String(repeating: "x",
                count: AtlasReaderSelectionClipboard.maximumPayloadBytes + 1)
            precondition(AtlasReaderSelectionClipboard.publish(oversized, to: pasteboard,
                expectedChangeCount: ownership) == .tooLarge)
            precondition(pasteboard.changeCount == ownership,
                         "a refused copy took clipboard ownership")
            assertText("preserve me", on: pasteboard)
        }
    }

    static func changedOwnershipPreservesTheNewerContents() {
        withPasteboard { pasteboard in
            let obsolete = seed("original", on: pasteboard)
            // Another named handle exercises the actual AppKit ownership count;
            // this is a deterministic competing write, not a cross-process race.
            let other = NSPasteboard(name: pasteboard.name)
            let newer = seed("newer user's copy", on: other)
            precondition(newer != obsolete)
            precondition(AtlasReaderSelectionClipboard.publish("obsolete selected text", to: pasteboard,
                expectedChangeCount: obsolete) == .changed)
            precondition(pasteboard.changeCount == newer)
            assertText("newer user's copy", on: pasteboard)
        }
    }

    static func exactBudgetBoundaryPublishesWithoutTruncation() {
        withPasteboard { pasteboard in
            let ownership = seed("previous", on: pasteboard)
            let value = String(repeating: "x", count: AtlasReaderSelectionClipboard.maximumPayloadBytes)
            precondition(value.utf8.count * 2 == AtlasReaderSelectionClipboard.aggregateByteLimit)
            precondition(AtlasReaderSelectionClipboard.publish(value, to: pasteboard,
                expectedChangeCount: ownership) == .copied)
            assertText(value, on: pasteboard)
        }
    }

    static func budgetCountsUTF8BytesRatherThanCharacters() {
        withPasteboard { pasteboard in
            let ownership = seed("untouched", on: pasteboard)
            let emoji = String(repeating: "🦀",
                count: AtlasReaderSelectionClipboard.maximumPayloadBytes / 4 + 1)
            precondition(emoji.count < AtlasReaderSelectionClipboard.maximumPayloadBytes)
            precondition(AtlasReaderSelectionClipboard.publish(emoji, to: pasteboard,
                expectedChangeCount: ownership) == .tooLarge)
            precondition(pasteboard.changeCount == ownership)
            assertText("untouched", on: pasteboard)
        }
    }

    static func emptyLogicalTextIsAnExplicitSuccessfulCopy() {
        withPasteboard { pasteboard in
            let ownership = seed("previous", on: pasteboard)
            precondition(AtlasReaderSelectionClipboard.publish("", to: pasteboard,
                expectedChangeCount: ownership) == .copied)
            assertText("", on: pasteboard)
            precondition(pasteboard.pasteboardItems?.count == 1)
        }
    }
}
