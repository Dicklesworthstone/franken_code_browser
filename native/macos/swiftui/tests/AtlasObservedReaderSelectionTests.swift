import Foundation

/// Exercises the production selection decoder and model with injected receipts.
/// Rust owns decoding/source mapping; native presentation remains a Mac lane.
@main private enum AtlasObservedReaderSelectionTests {
    private static func rejects(_ label: String, _ work: () throws -> Void) {
        do { try work(); preconditionFailure("Accepted invalid selection: " + label) }
        catch AtlasObservedReaderError.invalidInput { }
        catch AtlasObservedReaderError.invalidResponse { }
        catch { preconditionFailure("Unexpected error for " + label + ": \(error)") }
    }
    @MainActor private static func wait(_ label: String, _ ready: () -> Bool) async {
        for _ in 0..<4000 {
            if ready() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure("Timed out: " + label)
    }
    private static func wireTests() throws {
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("a.swift".utf8))
        let owner: UInt64 = 0x8000000000000007, offset: UInt64 = (1 << 33) + 8
        for encoding in AtlasObservedReaderEncoding.allCases {
            let text = "a😀e\u{301}\r\nz"
            let bytes = AtlasObservedReaderSearchRequest.originalBytes(text, encoding: encoding)
            let request = try AtlasObservedReaderRequest(offset: offset, bytes: UInt64(bytes.count), encoding: encoding)
            let raw = ObservedReaderFixture.page(owner, input, 7, request, length: offset + UInt64(bytes.count) + 16,
                text: text, originalHex: ObservedReaderFixture.hex(bytes))
            let page = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(raw), handle: owner, input: input, request: request)
            let native = (text as NSString).range(of: "😀e\u{301}\r\n")
            let selection = try AtlasObservedReaderSelectionRequest(page: page, nativeRange: native)
            let prefix = AtlasObservedReaderSearchRequest.originalBytes("a", encoding: encoding)
            let selected = AtlasObservedReaderSearchRequest.originalBytes("😀e\u{301}\r\n", encoding: encoding)
            let originalStart = offset + UInt64(prefix.count), originalEnd = originalStart + UInt64(selected.count)
            let receipt = ObservedReaderFixture.selection(raw, utf8Start: selection.utf8Start, utf8End: selection.utf8End,
                originalStart: originalStart, originalEnd: originalEnd, text: selection.text,
                originalHex: ObservedReaderFixture.hex(selected))
            let accepted = try AtlasObservedReaderSelection.decode(ObservedReaderFixture.encode(receipt), input: input, request: selection)
            precondition(accepted.originalRange.start == originalStart && accepted.originalRange.end == originalEnd)
            precondition(accepted.text == "😀e\u{301}\r\n" && !accepted.containsReplacements)
            precondition(accepted.withLocationText.contains("\"whole_file_immutable\":false"))
            precondition(accepted.withLocationText.contains("\"extent_sha256\"") && !accepted.withLocationText.contains("\"capture_sha256\""))
            precondition(accepted.withLocationText.contains(String(originalStart)), "Full-width original offsets must survive provenance")

            let mutations: [(String, (inout [String: Any]) -> Void)] = [
                ("wrong observation", { $0["observation"] = "8" }),
                ("wrong extent digest", { $0["extent_sha256"] = String(repeating: "b", count: 64) }),
                ("wrong native range", { $0["window_utf8_range"] = ObservedReaderFixture.range(0, selection.utf8End) }),
                ("wrong page", { $0["window_original_range"] = ObservedReaderFixture.range(offset + 1, offset + UInt64(bytes.count)) }),
                ("whole file claim", { $0["whole_file_immutable"] = true }),
                ("reread", { $0["additional_source_bytes_read"] = "1" }),
                ("changed selected text", { $0["text"] = "new" }),
                ("changed selected bytes", { $0["original_hex"] = String(repeating: "00", count: selected.count) }),
                ("inconsistent replacement flag", { $0["contains_replacements"] = true }),
                ("wrong namespace", { $0["selection_namespace"] = "native-glyph" }),
            ]
            for (label, mutate) in mutations {
                var changed = receipt; mutate(&changed)
                rejects(label) { _ = try AtlasObservedReaderSelection.decode(ObservedReaderFixture.encode(changed), input: input, request: selection) }
            }
            for range in [NSRange(location: NSNotFound, length: 1), NSRange(location: 0, length: 0),
                          NSRange(location: 2, length: 1), NSRange(location: 4, length: 1),
                          NSRange(location: Int.max, length: 1)] {
                rejects("invalid native boundary") { _ = try AtlasObservedReaderSelectionRequest(page: page, nativeRange: range) }
            }
        }

        // Identical words do not permit a range from another location.
        let repeated = "word word", request = try AtlasObservedReaderRequest(offset: 0, bytes: 9)
        let raw = ObservedReaderFixture.page(owner, input, 8, request, length: 9, text: repeated)
        let page = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(raw), handle: owner, input: input, request: request)
        let selected = try AtlasObservedReaderSelectionRequest(page: page, nativeRange: NSRange(location: 5, length: 4))
        let correct = ObservedReaderFixture.selection(raw, utf8Start: 5, utf8End: 9,
            originalStart: 5, originalEnd: 9, text: "word", originalHex: "776f7264")
        _ = try AtlasObservedReaderSelection.decode(ObservedReaderFixture.encode(correct), input: input, request: selected)
        var shifted = correct; shifted["original_range"] = ObservedReaderFixture.range(0, 4)
        rejects("equal text with different original offsets") {
            _ = try AtlasObservedReaderSelection.decode(ObservedReaderFixture.encode(shifted), input: input, request: selected)
        }

        // Malformed input keeps its exact original byte while text is explicit.
        let malformedRequest = try AtlasObservedReaderRequest(offset: 0, bytes: 3)
        var malformed = ObservedReaderFixture.page(owner, input, 9, malformedRequest,
            length: 3, text: "a\u{fffd}z", originalHex: "61ff7a")
        malformed["has_replacements"] = true
        let malformedPage = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(malformed),
            handle: owner, input: input, request: malformedRequest)
        let malformedSelection = try AtlasObservedReaderSelectionRequest(page: malformedPage, nativeRange: NSRange(location: 1, length: 1))
        let malformedReceipt = ObservedReaderFixture.selection(malformed, utf8Start: 1, utf8End: 4,
            originalStart: 1, originalEnd: 2, text: "\u{fffd}", originalHex: "ff", replacements: true)
        let decoded = try AtlasObservedReaderSelection.decode(ObservedReaderFixture.encode(malformedReceipt),
            input: input, request: malformedSelection)
        precondition(decoded.originalHex == "ff" && decoded.containsReplacements)

        // UTF-16 BOM bytes are outside the visible decoded window, never
        // silently prepended to a selected logical passage.
        let bomRequest = try AtlasObservedReaderRequest(offset: 0, bytes: 8, encoding: .utf16le)
        var bom = ObservedReaderFixture.page(owner, input, 11, bomRequest, length: 8,
            text: "A😀", originalHex: "41003dd800de")
        bom["visible_range"] = ObservedReaderFixture.range(2, 8); bom["boundaries_adjusted"] = true
        let bomPage = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(bom),
            handle: owner, input: input, request: bomRequest)
        let bomSelection = try AtlasObservedReaderSelectionRequest(page: bomPage, nativeRange: NSRange(location: 1, length: 2))
        let bomReceipt = ObservedReaderFixture.selection(bom, utf8Start: 1, utf8End: 5,
            originalStart: 4, originalEnd: 8, text: "😀", originalHex: "3dd800de")
        let bomSelected = try AtlasObservedReaderSelection.decode(ObservedReaderFixture.encode(bomReceipt),
            input: input, request: bomSelection)
        precondition(bomSelected.originalRange.start == 4 && bomSelected.originalHex == "3dd800de")

        // A line scan's retained receipt keeps its stronger line provenance,
        // without claiming that the source prefix scan was an atomic snapshot.
        let lines = try AtlasObservedReaderLineRequest(firstLine: 3, lines: 1, maximumSelectedBytes: 4, maximumScanBytes: 100)
        let lineRaw = try ObservedReaderFixture.linePage(owner, input, 10, lines, offset: 8, length: 100)
        let linePage = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(lineRaw), handle: owner, input: input, lines: lines)
        let lineSelection = try AtlasObservedReaderSelectionRequest(page: linePage, nativeRange: NSRange(location: 0, length: 3))
        let lineReceipt = ObservedReaderFixture.selection(lineRaw, utf8Start: 0, utf8End: 3,
            originalStart: 8, originalEnd: 11, text: "old", originalHex: "6f6c64")
        _ = try AtlasObservedReaderSelection.decode(ObservedReaderFixture.encode(lineReceipt), input: input, request: lineSelection)
        var missingLines = lineReceipt; missingLines.removeValue(forKey: "line_navigation")
        rejects("missing line-scan provenance") {
            _ = try AtlasObservedReaderSelection.decode(ObservedReaderFixture.encode(missingLines), input: input, request: lineSelection)
        }
    }

    @MainActor private static func modelTests() async throws {
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("large.txt".utf8))
        let probe = ObservedReaderProbe(), model = AtlasObservedReaderModel(transport: probe.transport)
        model.pageBytes = 4
        model.open(.init(input: input, project: UUID(), access: nil))
        await wait("initial observation") { model.page != nil && !model.busy }
        let source = model.source!, navigation = model.navigation, originalReads = probe.windowOffsets.count
        model.updateTextSelection(source: source, navigation: navigation, range: NSRange(location: 0, length: 3))
        await wait("selected original bytes") { model.canCopySelection }
        precondition(model.textSelection?.text == "old" && model.textSelection?.originalHex == "6f6c64")
        precondition(probe.windowOffsets.count == originalReads && probe.copyCount == 0,
            "Mapping a retained selection performs no source read or whole-page copy")
        probe.setContent("new\n")
        model.updateTextSelection(source: source, navigation: navigation, range: NSRange(location: 1, length: 2))
        await wait("retained after live change") { model.canCopySelection && model.textSelection?.text == "ld" }
        precondition(probe.windowOffsets.count == originalReads)

        model.updateTextSelection(source: source, navigation: navigation, range: nil)
        precondition(!model.canCopySelection && model.textSelection == nil && !model.hasTextSelectionIntent)
        let gate = ObservedReaderGate()
        probe.blockSelection(0, on: gate)
        model.updateTextSelection(source: source, navigation: navigation, range: NSRange(location: 0, length: 1))
        await wait("selection entered") { gate.entered }
        model.updateTextSelection(source: source, navigation: navigation, range: NSRange(location: 1, length: 1))
        model.updateTextSelection(source: source, navigation: navigation, range: NSRange(location: 2, length: 1))
        model.updateTextSelection(source: source, navigation: navigation, range: NSRange(location: 3, length: 1))
        gate.release()
        await wait("latest selection only") { model.canCopySelection && model.textSelection?.originalRange.start == 3 }
        precondition(Array(probe.requestedSelectionStarts.suffix(2)) == [0, 3])
        precondition(probe.maximumActive == 1, "Dragging never overlaps observed-reader operations")

        // Foreground navigation supersedes selection work and prevents the
        // prior observation's otherwise-valid receipt from attaching later.
        let navigationGate = ObservedReaderGate()
        probe.blockSelection(1, on: navigationGate)
        model.updateTextSelection(source: source, navigation: navigation, range: NSRange(location: 1, length: 1))
        await wait("selection before navigation") { navigationGate.entered }
        model.observe(offset: 4)
        precondition(!model.canCopySelection)
        navigationGate.release()
        await wait("new observed page") { model.page?.visibleRange.start == 4 && !model.busy }
        precondition(model.textSelection == nil && !model.hasTextSelectionIntent)
        model.updateTextSelection(source: source, navigation: navigation, range: NSRange(location: 0, length: 1))
        precondition(model.textSelection == nil, "An old native view cannot select into a newer observation")

        let current = model.source!, currentNavigation = model.navigation
        model.updateTextSelection(source: current, navigation: currentNavigation, range: NSRange(location: 0, length: 3))
        await wait("new selected source") { model.canCopySelection }
        precondition(model.textSelection?.text == "new" && model.textSelection?.originalRange.start == 4)
        let owner = model.page!.observation.identity.owner, observation = model.page!.observation.id
        probe.evict(owner, observation)
        model.updateTextSelection(source: current, navigation: currentNavigation, range: NSRange(location: 1, length: 1))
        await wait("evicted selection refusal") { !model.resolvingTextSelection }
        precondition(model.textSelection == nil && !model.canCopySelection)
        model.close()
        await wait("model retirement") { probe.closedCount == probe.createCount }
    }

    @MainActor private static func cancellationAndLegacy() async throws {
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("a.txt".utf8))
        let probe = ObservedReaderProbe()
        let model = AtlasObservedReaderModel(transport: probe.transport)
        model.pageBytes = 4
        model.open(.init(input: input, project: UUID(), access: nil))
        await wait("open for clear") { model.page != nil && !model.busy }
        let gate = ObservedReaderGate()
        probe.blockSelection(0, on: gate)
        model.updateTextSelection(source: model.source!, navigation: model.navigation, range: NSRange(location: 0, length: 1))
        await wait("clear pending entered") { gate.entered }
        model.updateTextSelection(source: model.source!, navigation: model.navigation, range: nil)
        precondition(!model.resolvingTextSelection && model.textSelection == nil)
        model.close()
        gate.release()
        await wait("close drains selection") { probe.closedCount == probe.createCount }
        precondition(model.textSelection == nil && model.closed && probe.maximumActive == 1)

        var legacy = probe.transport; legacy.resolveSelection = nil
        let old = AtlasObservedReaderModel(transport: legacy); old.pageBytes = 4
        old.open(.init(input: input, project: UUID(), access: nil))
        await wait("legacy open") { old.page != nil && !old.busy }
        old.updateTextSelection(source: old.source!, navigation: old.navigation, range: NSRange(location: 0, length: 3))
        precondition(old.textSelection == nil && !old.resolvingTextSelection
            && old.textSelectionNotice.contains("unavailable"))
        precondition(old.canCopy, "Legacy whole-observation copy remains available")
        old.close()
        await wait("legacy close") { probe.closedCount == probe.createCount }
    }

    @MainActor private static func failedAndEditedNavigation() async throws {
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("large.txt".utf8))
        let probe = ObservedReaderProbe(), searchGate = ObservedReaderGate()
        var transport = probe.transport
        transport.search = .init(find: { _, _, _ in
            searchGate.wait()
            throw AtlasObservedReaderError.canceled
        }, hit: { _, _, _ in nil })
        let model = AtlasObservedReaderModel(transport: transport); model.pageBytes = 4
        model.open(.init(input: input, project: UUID(), access: nil))
        await wait("open for navigation retry") { model.page != nil && !model.busy }
        let source = model.source!, navigation = model.navigation, observation = model.page!.observation
        model.updateTextSelection(source: source, navigation: navigation, range: NSRange(location: 0, length: 3))
        await wait("selection before failed line scan") { model.canCopySelection }

        probe.failLine("OBSERVED_READER_LINE_SCAN_BYTE_LIMIT")
        model.lineInput = "9"; model.lineCountInput = "1"; model.observeLineInput()
        await wait("selection after failed line scan") { !model.busy && model.canCopySelection }
        precondition(model.page?.observation == observation && model.textSelection?.text == "old",
            "Failed navigation re-resolves the still-displayed selection rather than copying a whole page")

        let lines = ObservedReaderGate(); probe.blockLine(10, on: lines)
        model.lineInput = "10"; model.observeLineInput()
        await wait("line scan entered") { lines.entered }
        model.lineInput = "11"
        precondition(!model.busy && !model.canCopySelection)
        lines.release()
        await wait("selection after line-input cancellation") { model.canCopySelection }
        precondition(model.page?.observation == observation && model.textSelection?.text == "old")

        model.searchText = "old"; model.find()
        await wait("search entered before mode edit") { searchGate.entered }
        model.searchMode = .occurrences
        precondition(!model.busy && !model.canCopySelection && !model.searchAuthorized)
        searchGate.release()
        await wait("selection after search-mode cancellation") { model.canCopySelection }
        precondition(model.page?.observation == observation && model.textSelection?.text == "old")
        precondition(probe.maximumActive == 1)
        model.close()
        await wait("navigation-retry owner retired") { probe.closedCount == probe.createCount }
    }

    @MainActor static func main() async throws {
        try wireTests()
        try await modelTests()
        try await cancellationAndLegacy()
        try await failedAndEditedNavigation()
        print("Observed selection mapping, original-byte correlation, provenance, latest drag, cancellation and legacy fixtures passed.")
    }
}
