import AppKit

/// One bounded, explicit publication of already resolved logical text. Source
/// mapping and stale-reader checks belong to the caller, before this operation.
@MainActor enum AtlasReaderSelectionClipboard {
    /// Charge the UTF-8 payload twice: caller representation plus native
    /// staging. This is an admission bound, not a process/OS memory measurement.
    static let aggregateByteLimit = 2 * 1024 * 1024
    static let maximumPayloadBytes = aggregateByteLimit / 2

    enum Outcome: Equatable {
        case copied, tooLarge, changed, failed
    }

    /// Refusal before publication leaves the pasteboard untouched. Stage the
    /// complete advertised .string flavor before clearing existing contents,
    /// and observe intervening ownership changes before publication.
    ///
    /// AppKit exposes no compare-and-swap publication: another process can
    /// still race the final check. A native failure after clearContents may
    /// have changed the pasteboard. Never attempt rollback over newer content.
    static func publish(_ value: String, to pasteboard: NSPasteboard,
                        expectedChangeCount: Int) -> Outcome {
        // Inspect at most the allowance plus one byte even for an oversized
        // caller. Do not allocate a second Data/String just to count or clip it.
        guard value.utf8.prefix(maximumPayloadBytes + 1).count <= maximumPayloadBytes else {
            return .tooLarge
        }
        guard pasteboard.changeCount == expectedChangeCount else { return .changed }
        let item = NSPasteboardItem()
        guard item.setString(value, forType: .string) else { return .failed }
        guard pasteboard.changeCount == expectedChangeCount else { return .changed }

        pasteboard.clearContents()
        return pasteboard.writeObjects([item]) ? .copied : .failed
    }
}
