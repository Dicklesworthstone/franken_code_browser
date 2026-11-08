import Foundation

@main private enum AtlasObservedReaderTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: Bool, _ label: String) {
        precondition(value, label); checks += 1
    }
    @MainActor private static func wait(_ label: String, _ ready: () -> Bool) async {
        for _ in 0..<4_000 {
            if ready() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure(label)
    }
    @MainActor private static func rejectInput(_ body: () throws -> Void) {
        do { try body(); preconditionFailure("Invalid observed-reader input accepted") }
        catch AtlasObservedReaderError.invalidInput { checks += 1 }
        catch { preconditionFailure("Unexpected invalid-input error: \(error)") }
    }
    @MainActor private static func rejectWire(_ body: () throws -> Void) {
        do { try body(); preconditionFailure("Contradictory observation accepted") }
        catch AtlasObservedReaderError.invalidResponse { checks += 1 }
        catch { preconditionFailure("Unexpected wire error: \(error)") }
    }
    @MainActor private static func source(_ input: AtlasObservedReaderInput, access: AnyObject? = nil) -> AtlasObservedReaderSource {
        .init(input: input, project: UUID(), access: access)
    }

    @MainActor static func main() async throws {
        try inputAndWire()
        try await retainedNavigation()
        try await latestAndReopen()
        try await cancellationAndGrantDrain()
        try await quarantineAndCapacity()
        print("AtlasObservedReaderTests: \(checks) checks passed")
    }

    @MainActor private static func inputAndWire() throws {
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("large.log".utf8))
        let request = try AtlasObservedReaderRequest(offset: 0, bytes: 4)
        check(input.absolutePath == Array("/repo/large.log".utf8), "Root-relative bytes are joined without display-label authority")
        let raw = try AtlasObservedReaderInput(root: "/repo", relativePath: [102, 255, 10])
        check(raw.relativeHex == "66ff0a" && raw.absolutePath == [47, 114, 101, 112, 111, 47, 102, 255, 10],
            "Non-UTF8 and control-containing names retain exact reversible bytes")
        let composed = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("é.swift".utf8))
        let decomposed = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("e\u{301}.swift".utf8))
        check(composed != decomposed, "Canonical Unicode equivalence does not merge native path identity")
        for root in ["repo", "", "/repo/", "/repo//dir", "/repo/../other", "/repo/.", "/re\0po"] {
            rejectInput { _ = try AtlasObservedReaderInput(root: root, relativePath: [97]) }
        }
        let invalidPaths: [[UInt8]] = [[], [47, 97], [97, 47], [46], [46, 46], [97, 47, 47, 98], [97, 47, 46, 46, 47, 98], [97, 0, 98]]
        for path in invalidPaths {
            rejectInput { _ = try AtlasObservedReaderInput(root: "/repo", relativePath: path) }
        }
        rejectInput { _ = try AtlasObservedReaderInput(root: "/", relativePath: Array(repeating: 97, count: 16_384)) }
        rejectInput { _ = try AtlasObservedReaderInput(root: "/", relativePath: Array(Array(repeating: "a", count: 257).joined(separator: "/").utf8)) }
        for bytes in [UInt64(0), 3, AtlasObservedReaderLimits.maximumPageBytes + 1] {
            rejectInput { _ = try AtlasObservedReaderRequest(offset: 0, bytes: bytes) }
        }
        rejectInput { _ = try AtlasObservedReaderRequest(offset: UInt64.max, bytes: 4) }

        let infoObject = ObservedReaderFixture.info(41, input)
        let info = try AtlasObservedReaderInfo.decode(ObservedReaderFixture.encode(infoObject), handle: 41, input: input, initial: true)
        check(info.initialLength > 1 << 32 && info.sourceBytesRead == 0 && info.retained.isEmpty,
            "Metadata-only open admits large files without a source prefix capture")
        for key in ["initial_observed_length", "payload_bytes_read", "read_calls", "cache_capacity", "retained_payload_bytes", "retained_observations"] {
            var bad = infoObject; bad.removeValue(forKey: key)
            rejectWire { _ = try AtlasObservedReaderInfo.decode(ObservedReaderFixture.encode(bad), handle: 41, input: input, initial: true) }
        }
        for (key, value) in [("payload_bytes_read", "1"), ("read_calls", "1"), ("cache_capacity", "9"), ("retained_payload_bytes", "1")] {
            var bad = infoObject; bad[key] = value
            rejectWire { _ = try AtlasObservedReaderInfo.decode(ObservedReaderFixture.encode(bad), handle: 41, input: input, initial: true) }
        }
        var duplicateIDs = infoObject
        duplicateIDs["retained_observations"] = ["2", "2"]
        rejectWire { _ = try AtlasObservedReaderInfo.decode(ObservedReaderFixture.encode(duplicateIDs), handle: 41, input: input) }

        let object = ObservedReaderFixture.page(41, input, 1, request)
        let page = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(object), handle: 41, input: input, request: request)
        check(page.observation.id == 1 && page.visibleRange.count == 4 && page.nextOffset == 4
            && page.observation.capturedRange.count == 12 && !page.observation.wholeFileObserved,
            "Visible bytes, context bytes and whole observed length remain separate")
        let farRequest = try AtlasObservedReaderRequest(offset: (1 << 32) + 4, bytes: 4)
        let far = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(
            ObservedReaderFixture.page(41, input, 2, farRequest)), handle: 41, input: input, request: farRequest)
        check(far.visibleRange.start == (1 << 32) + 4 && far.firstPhysicalLine == nil,
            "Far byte navigation preserves UInt64 offsets and does not claim a global line")
        for offset in [UInt64(1), 2, 4] {
            let request = try AtlasObservedReaderRequest(offset: offset, bytes: 4)
            // The retained prefix consists of offset newline bytes followed
            // by "abcd"; only the four visible bytes are returned as text.
            var early = ObservedReaderFixture.page(41, input, offset + 3, request, length: offset + 4, text: "abcd")
            early["first_physical_line"] = String(offset + 1)
            let page = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(early), handle: 41, input: input, request: request)
            check(page.firstPhysicalLine == offset + 1,
                "An observed prefix beginning at zero may prove early line \(offset + 1)")
        }
        for encoding in [AtlasObservedReaderEncoding.utf16le, .utf16be] {
            let request = try AtlasObservedReaderRequest(offset: 0, bytes: 4, encoding: encoding)
            let hex = encoding == .utf16le ? "41004200" : "00410042"
            let page = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(
                ObservedReaderFixture.page(41, input, 10, request, text: "AB", originalHex: hex)),
                handle: 41, input: input, request: request)
            check(page.text == "AB" && page.originalHex == hex && page.observation.request.encoding == encoding,
                "Explicit UTF16 decoding does not replace original observed bytes")
        }
        let empty = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(
            ObservedReaderFixture.page(41, input, 11, request, length: 0, text: "")),
            handle: 41, input: input, request: request)
        check(empty.visibleRange.count == 0 && empty.firstPhysicalLine == nil && empty.nextOffset == nil,
            "Empty observed EOF does not invent a physical line or another page")

        let badFields: [(String, Any)] = [
            ("schema", "fcb.reader-session/1"), ("owner", "42"), ("file_id", "2"), ("command", "retained-window"),
            ("capture_kind", "complete-capture"), ("whole_file_immutable", true), ("native_presented", true),
            ("capture_sha256", String(repeating: "a", count: 64)), ("source_revision", "1"),
            ("observation", "0"), ("observation", "01"), ("observation", 1), ("encoding", "automatic"),
            ("encoding_basis", "inherited-bom"), ("requested_offset", "1"), ("requested_bytes", "5"),
            ("captured_bytes", "13"), ("extent_sha256", String(repeating: "A", count: 64)),
            ("whole_file_observed", true), ("requested_capture_filled", false),
            ("observation_source_bytes_read", "0"), ("observation_read_calls", "0"), ("observation_read_calls", "4097"),
            ("additional_source_bytes_read", "0"), ("additional_read_calls", "0"),
            ("final_observed_length", "3"), ("consistency", "atomic"), ("text_kind", "whole-file-shaped-text"),
            ("boundaries_adjusted", true), ("original_hex", "6f6c64"), ("first_physical_line", "2"),
            ("next_offset", NSNull()), ("next_offset", "0"), ("visible_range", ObservedReaderFixture.range(0, 5))]
        for (key, value) in badFields {
            var bad = object; bad[key] = value
            rejectWire { _ = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(bad), handle: 41, input: input, request: request) }
        }
        for key in ["owner", "observation", "encoding_basis", "requested_visible_range", "requested_capture_range", "captured_range", "original_hex"] {
            var bad = object; bad.removeValue(forKey: key)
            rejectWire { _ = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(bad), handle: 41, input: input, request: request) }
        }
        var wrongPath = object
        wrongPath["path"] = ["encoding": "unix-bytes", "hex": ObservedReaderFixture.hex(decomposed.absolutePath), "display": input.displayPath]
        rejectWire { _ = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(wrongPath), handle: 41, input: input, request: request) }
        var changed = object; changed["consistency"] = "changed-during-read"; changed["final_observed_length"] = "99"
        let changedPage = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(changed), handle: 41, input: input, request: request)
        check(changedPage.observation.finalObservedLength == 99 && !changedPage.observation.wholeFileObserved,
            "Metadata change is explicit instead of silently upgraded to immutable whole-file evidence")

        let retained = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(ObservedReaderFixture.retained(object)),
            handle: 41, input: input, retained: page)
        check(retained.matches(page), "Retained history restores the identical observation and zero additional source reads")
        var alteredRetained = ObservedReaderFixture.retained(object); alteredRetained["text"] = "new\n"
        rejectWire { _ = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(alteredRetained), handle: 41, input: input, retained: page) }
        var reRead = ObservedReaderFixture.retained(object); reRead["additional_read_calls"] = "1"
        rejectWire { _ = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(reRead), handle: 41, input: input, retained: page) }
        let copied = try AtlasObservedReaderCopy.decode(ObservedReaderFixture.encode(ObservedReaderFixture.copy(object)), handle: 41, input: input, page: page)
        check(copied.originalHex == page.originalHex, "Copy is bound to the retained observation and exact original range")
        for (key, value) in [("observation", "2"), ("original_hex", "6e65770a"), ("additional_source_bytes_read", "4")] {
            var bad = ObservedReaderFixture.copy(object); bad[key] = value
            rejectWire { _ = try AtlasObservedReaderCopy.decode(ObservedReaderFixture.encode(bad), handle: 41, input: input, page: page) }
        }
        rejectWire { _ = try AtlasObservedReaderPage.decode(nil, handle: 41, input: input, request: request) }
        do {
            _ = try AtlasObservedReaderPage.decode(ObservedReaderFixture.failure(41, "OBSERVED_READER_OBSERVATION_UNAVAILABLE"),
                handle: 41, input: input, retained: page)
            preconditionFailure("Eviction was silently accepted")
        } catch AtlasObservedReaderError.engine(let code) {
            check(code == "OBSERVED_READER_OBSERVATION_UNAVAILABLE", "Evicted history remains explicitly unavailable")
        }
    }

    @MainActor private static func retainedNavigation() async throws {
        let probe = ObservedReaderProbe()
        let reader = AtlasObservedReaderModel(transport: probe.transport)
        reader.pageBytes = 4
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: [97])
        reader.open(source(input))
        await wait("Initial range never appeared") { !reader.busy && reader.page != nil }
        let original = reader.page!
        check(original.observation.request.offset == 0 && probe.openCount == 1, "Initial open observes one bounded first range")
        probe.setContent("new\n")
        reader.nextRange()
        await wait("Next range never appeared") { !reader.busy && reader.page?.observation.request.offset == 4 }
        check(reader.page?.text == "new\n" && reader.page?.observation.id != original.observation.id,
            "A later range has its own content and observation identity")
        let reads = probe.windowOffsets
        reader.back()
        await wait("Back never restored retained page") { !reader.busy && reader.page?.observation.id == original.observation.id }
        check(reader.page?.text == "old\n" && probe.windowOffsets == reads && probe.restoreCount == 1,
            "Back preserves old bytes after later content observations without a live reread")
        let copied = await reader.copyOriginalHex()
        check(copied == original.originalHex && probe.windowOffsets == reads && probe.copyCount == 1,
            "Original byte copy names the old retained observation")
        reader.forward()
        await wait("Forward never restored retained page") { !reader.busy && reader.page?.text == "new\n" }
        let accepted = reader.page!
        probe.evict(accepted.observation.identity.owner, accepted.observation.id)
        let missing = await reader.copyOriginalHex()
        check(missing == nil && reader.page?.matches(accepted) == true && probe.windowOffsets == reads,
            "Evicted copy refuses without substituting live source or discarding the accepted display")
        reader.close()
        await wait("Reader did not retire") { probe.closedCount == probe.createCount }
    }

    @MainActor private static func latestAndReopen() async throws {
        let probe = ObservedReaderProbe()
        let reader = AtlasObservedReaderModel(transport: probe.transport)
        reader.pageBytes = 4
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: [97])
        reader.open(source(input))
        await wait("Initial latest-only reader never opened") { !reader.busy && reader.page != nil }
        let gate = ObservedReaderGate(); probe.block(4, on: gate)
        reader.observe(offset: 4)
        await wait("Blocked read never entered") { gate.entered }
        reader.observe(offset: 8)
        await Task.yield()
        reader.observe(offset: 12)
        await Task.yield()
        gate.release()
        await wait("Latest observation never committed") { !reader.busy && reader.page?.observation.request.offset == 12 }
        check(probe.maximumActive == 1 && probe.windowOffsets == [0, 4, 12],
            "One active/one latest queue coalesces obsolete windows instead of reading every request")
        let accepted = reader.page!, navigation = reader.navigation
        probe.failOpen(); reader.reopenCurrentFile()
        await wait("Failed reopen did not finish") { !reader.busy }
        check(reader.page?.matches(accepted) == true && reader.navigation == navigation,
            "Reopen failure preserves the previous descriptor, range and navigation")
        probe.setContent("next")
        reader.reopenCurrentFile()
        await wait("Explicit reopen never committed") { !reader.busy && reader.page?.text == "next" }
        check(reader.page?.observation.identity.owner != accepted.observation.identity.owner && !reader.canBack,
            "Successful explicit reopen commits a new owner and clears only observed-range history")
        reader.close()
        await wait("Reopened descriptors did not retire") { probe.closedCount == probe.createCount }
    }

    @MainActor private static func cancellationAndGrantDrain() async throws {
        let probe = ObservedReaderProbe()
        let model = AtlasObservedReaderModel(transport: probe.transport)
        model.pageBytes = 4
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: [97])
        var grant: ObservedReaderGrant? = ObservedReaderGrant(probe, "draining")
        model.open(source(input, access: grant)); grant = nil
        await wait("Grant reader never opened") { !model.busy && model.page != nil }
        let accepted = model.page!
        let gate = ObservedReaderGate(); probe.block(4, on: gate)
        model.observe(offset: 4)
        await wait("Cancelable read never entered") { gate.entered }
        model.cancel()
        check(model.page?.matches(accepted) == true && !model.busy, "Cancel preserves the accepted immutable observation")
        model.close()
        check(!probe.hasReleased("draining") && probe.closedCount == 0,
            "Closing keeps the source grant until the entered worker returns")
        gate.release()
        await wait("Grant was not released after confirmed close") { probe.hasReleased("draining") && probe.closedCount == 1 }
        check(model.page == nil && model.closed, "A valid late reply cannot resurrect a closed observed reader")

        let viewSource = source(input)
        let presentation = AtlasObservedReaderPresentation(source: viewSource, transport: probe.transport)
        presentation.close(); presentation.start()
        check(presentation.model.closed && probe.createCount == 1,
            "An old sheet appearance cannot reopen a presentation after authority revocation")
        let a = AtlasObservedReaderSource(input: input, project: viewSource.identity.project, access: nil)
        let b = AtlasObservedReaderSource(input: input, project: UUID(), access: nil)
        check(a.identity != b.identity, "Project identity changes revoke otherwise identical path requests")
    }

    @MainActor private static func quarantineAndCapacity() async throws {
        let probe = ObservedReaderProbe()
        probe.setCloseAllowed(false)
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: [97])
        var readers: [AtlasObservedReaderModel] = []
        for index in 0..<4 {
            let model = AtlasObservedReaderModel(transport: probe.transport)
            model.pageBytes = 4
            model.open(source(input, access: ObservedReaderGrant(probe, "quarantine-\(index)")))
            await wait("Quarantine setup exhausted capacity early") { !model.busy && model.page != nil }
            readers.append(model)
        }
        for reader in readers { reader.close() }
        readers = []
        await wait("Failed closes were not attempted") { probe.failedCloseOwners == 4 }
        check((0..<4).allSatisfy { !probe.hasReleased("quarantine-\($0)") } && probe.closedCount == 0,
            "Every failed retirement keeps its native grant and admission permit")
        let refused = AtlasObservedReaderModel(transport: probe.transport)
        refused.pageBytes = 4
        for _ in 0..<24 { refused.open(source(input)) }
        check(probe.createCount == 4 && refused.page == nil,
            "Repeated fifth-open refusals cannot decrement permits or create additional registry handles")
        check((0..<4).allSatisfy { !probe.hasReleased("quarantine-\($0)") },
            "Repeated refused openings do not release quarantined grants")
        probe.setCloseAllowed(true)
        await wait("Explicit retry never released quarantined grants") {
            // Explicit test requests coalesce while an earlier refused pass
            // drains, then admit a fresh pass after close becomes available.
            AtlasObservedReaderModel.retryRetirements()
            return probe.closedCount == 4 && (0..<4).allSatisfy { probe.hasReleased("quarantine-\($0)") }
        }
        await wait("Capacity did not recover after confirmed retirement") {
            // Grant and permit destruction can finish at adjacent moments on
            // the retirement lane; each explicit retry still admits at most
            // one request and cannot create a handle while permits are full.
            if !refused.busy && refused.page == nil { refused.reopenCurrentFile() }
            return !refused.busy && refused.page != nil
        }
        check(probe.createCount == 5 && probe.retirementFailures >= 4,
            "Confirmed close releases permits; a later explicit open can succeed")
        refused.close()
        await wait("Final recovery reader did not close") { probe.closedCount == 5 }
    }
}
