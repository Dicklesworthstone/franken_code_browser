import Foundation

@_silgen_name("selection_fixture_reset") private func resetSelectionFixture()
@_silgen_name("selection_fixture_mode") private func selectionFixtureMode(_ value: Int32)
@_silgen_name("selection_fixture_calls") private func selectionFixtureCalls() -> UInt64
@_silgen_name("selection_fixture_allocated") private func selectionFixtureAllocated() -> UInt64
@_silgen_name("selection_fixture_freed") private func selectionFixtureFreed() -> UInt64
@_silgen_name("selection_fixture_live") private func selectionFixtureLive() -> UInt64
// App.swift supplies the shared declaration in the production application.
@_silgen_name("fcb_free_string") func fcb_free_string(_ pointer: UnsafeMutablePointer<CChar>?)

/// Calls the actual production Swift bridge with header-checked C fixtures.
/// This verifies marshaling and owned buffer release, not Rust source semantics.
/// Run the native compiler/runner through the independent strict-RCH verifier.
@main private enum AtlasReaderTextSelectionBridgeTests {
    static func resolve() -> String? {
        AtlasReaderTextSelectionTransport.native.resolve(
            UInt64.max - 10, UInt64.max - 9, UInt64.max - 8, UInt64.max - 7, UInt64.max)
    }

    static func main() {
        resetSelectionFixture()
        precondition(AtlasReaderLimits.responseBytes == 4 * 1024 * 1024)
        let expected = "{\"fixture\":\"e\u{301} 🦀\\r\\n\\u0000\"}"
        guard let response = resolve() else { preconditionFailure("valid C buffer was refused") }
        precondition(response.utf8.elementsEqual(expected.utf8),
                     "native marshaling changed Unicode bytes or JSON escapes")
        precondition(selectionFixtureCalls() == 1 && selectionFixtureAllocated() == 1
            && selectionFixtureFreed() == 1 && selectionFixtureLive() == 0)

        selectionFixtureMode(1)
        precondition(resolve() == nil, "null C reply must remain unavailable")
        precondition(selectionFixtureCalls() == 2 && selectionFixtureAllocated() == 1
            && selectionFixtureFreed() == 1 && selectionFixtureLive() == 0,
            "null reply must not be passed to fcb_free_string")

        // Empty, invalid UTF-8 and oversized native replies are all rejected.
        // The C fixture asserts the exact allocation is freed once, including
        // every failure path after a non-null pointer was handed over.
        for mode in [Int32(2), 3, 4] {
            selectionFixtureMode(mode)
            let allocations = selectionFixtureAllocated()
            let frees = selectionFixtureFreed()
            precondition(resolve() == nil, "malformed or oversized C reply was admitted")
            precondition(selectionFixtureAllocated() == allocations + 1
                && selectionFixtureFreed() == frees + 1 && selectionFixtureLive() == 0)
        }

        selectionFixtureMode(5)
        guard let maximum = resolve() else { preconditionFailure("exact response limit was refused") }
        precondition(maximum.utf8.count == AtlasReaderLimits.responseBytes)
        precondition(maximum.utf8.allSatisfy { $0 == 120 },
                     "an exactly bounded response was truncated or rewritten")
        precondition(selectionFixtureCalls() == 6 && selectionFixtureAllocated() == 5
            && selectionFixtureFreed() == 5 && selectionFixtureLive() == 0)
        print("AtlasReaderTextSelectionBridge: full-width arguments, Unicode, null/empty/invalid/oversized replies and exact frees passed")
    }
}
