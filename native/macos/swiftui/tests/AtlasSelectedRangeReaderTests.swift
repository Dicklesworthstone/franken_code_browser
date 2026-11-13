import Foundation

private final class SelectedRangeGrant {
    let label: String
    let probe: ObservedReaderProbe
    init(_ label: String, _ probe: ObservedReaderProbe) { self.label = label; self.probe = probe }
    deinit { probe.released(label) }
}

/// The complete-reader lifecycle is exercised by AtlasSelectedFileReaderTests.
/// This owner adds that already accepted kind to the range-intake scenarios.
@MainActor private final class SelectedRangeReader {
    let presentation: AtlasObservedReaderPresentation?
    private var lease: AtlasNativeOpenLease?
    private(set) var closed = false
    init(retained lease: AtlasNativeOpenLease?) { self.lease = lease; presentation = nil }
    init(observed presentation: AtlasObservedReaderPresentation) { self.presentation = presentation }
    func close() { closed = true; lease = nil; presentation?.close() }
}

/// Production native intake, observed presentation/model and receipt decoding,
/// with injected worker receipts. This does not qualify AppKit or macOS grants.
@MainActor @main private enum AtlasSelectedRangeReaderTests {
    static func until(_ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(5)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 1_000_000) }
        precondition(condition(), "Selected-file range fixture did not drain")
    }
    static func make(_ probe: ObservedReaderProbe) -> AtlasNativeOpen<SelectedRangeReader> {
        probe.setContent(String(repeating: "x", count: Int(AtlasObservedReaderLimits.pageBytes)))
        let model = AtlasNativeOpen<SelectedRangeReader>(
            prepare: { url, _ in AtlasNativeOpenLease(SelectedRangeGrant(url.lastPathComponent, probe)) },
            resolve: { url, _, lease in
                precondition(!Thread.isMainThread)
                return .selectedFile(url, lease: lease,
                    observedBytes: url.lastPathComponent == "small" ? 4 : ObservedReaderFixture.length,
                    retainedLimit: 32 * 1024 * 1024)
            },
            capture: { target in
                switch target {
                case .file(_, let lease): return SelectedRangeReader(retained: lease)
                case .observedFile(let url, let lease):
                    let input = try AtlasObservedReaderInput(selectedFile: Array(url.path.utf8))
                    let source = try AtlasObservedReaderSource(selectedFile: input, access: lease)
                    let presentation = AtlasObservedReaderPresentation(source: source, transport: probe.transport)
                    try await presentation.prepareSelectedFile()
                    return SelectedRangeReader(observed: presentation)
                case .folder: preconditionFailure("A file-only choice acquired project authority")
                }
            }, retire: { $0.close() },
            failureMessage: { ($0 as? AtlasObservedReaderError)?.message ?? "File opening failed." })
        model.activate()
        return model
    }
    static func open(_ model: AtlasNativeOpen<SelectedRangeReader>, _ name: String) {
        model.open([URL(fileURLWithPath: "/selection/" + name)], origin: .external) { _ in
            preconditionFailure("A standalone large file was routed to project discovery")
        }
    }
    static func main() async throws {
        try exactFileInput()
        try await boundedFirstRangeAndExplicitReopen()
        try await failedFirstRangePreservesBothReaderKinds()
        try await canceledFirstRangeDrainsBeforeNewestChoice()
        try await closeDuringFirstRange()
        print("Selected-file range intake: exact-file authority, bounded first range, replacement, cancellation, reopen and grant-drain fixtures passed")
    }
    static func exactFileInput() throws {
        let project = try AtlasObservedReaderInput(root: "/selected", relativePath: [0xff, 0x2e, 0x72, 0x73])
        let selected = try AtlasObservedReaderInput(selectedFile: project.absolutePath)
        precondition(selected.route == .selectedFile && selected.root.isEmpty
            && selected.relativePath.isEmpty && selected.relativeHex.isEmpty
            && selected.absolutePath == project.absolutePath && selected != project
            && selected.absoluteHex == "2f73656c65637465642fff2e7273",
            "A raw exact-file path has a different authority route from the same project-relative label")
        let first = try AtlasObservedReaderSource(selectedFile: selected, access: nil)
        let second = try AtlasObservedReaderSource(selectedFile: selected, access: nil)
        precondition(first.identity != second.identity, "A later file choice must get a fresh authority epoch")
        do {
            _ = try AtlasObservedReaderSource(selectedFile: project, access: nil)
            preconditionFailure("Project input was silently accepted as an exact-file choice")
        } catch AtlasObservedReaderError.invalidInput { }
        let invalid: [[UInt8]] = [[], [47], Array("relative.rs".utf8), [47, 97, 0, 98],
            Array("/one//two".utf8), Array("/one/./two".utf8), Array("/one/../two".utf8), Array("/one/".utf8),
            Array(("/" + Array(repeating: "x", count: 257).joined(separator: "/")).utf8),
            [47] + Array(repeating: 97, count: AtlasObservedReaderLimits.maximumPathBytes)]
        for path in invalid {
            do {
                _ = try AtlasObservedReaderInput(selectedFile: path)
                preconditionFailure("Invalid standalone path was accepted")
            } catch AtlasObservedReaderError.invalidInput { }
        }
    }
    static func boundedFirstRangeAndExplicitReopen() async throws {
        let probe = ObservedReaderProbe(), intake = make(probe)
        open(intake, "large")
        try await until { intake.reader?.presentation != nil && !intake.isLoading }
        do {
            let presentation = intake.reader!.presentation!, model = presentation.model
            let page = model.page!, info = model.info!
            precondition(probe.openCount == 1 && probe.windowOffsets == [0]
                && info.initialLength == ObservedReaderFixture.length && info.sourceBytesRead == 0
                && info.readCalls == 0 && page.observation.request.bytes == 65536
                && page.observation.request.encoding == .utf8 && !page.observation.wholeFileObserved
                && page.observation.sourceBytesRead <= 65536 + 16,
                "The first screen must be a bounded observation, following a metadata-only open")
            precondition(presentation.source.input.route == .selectedFile
                && presentation.source.input.root.isEmpty && probe.openedInputs.allSatisfy { $0.route == .selectedFile })
            presentation.start()
            precondition(probe.openCount == 1, "Displaying a prepared reader must not open the path again")

            let offset: UInt64 = (1 << 32) + 123
            model.offsetInput = String(offset); model.observeInput()
            try await until { !model.busy && model.page?.visibleRange.start == offset }
            precondition(probe.windowOffsets == [0, offset] && model.page?.observation.identity.path == presentation.source.input.absolutePath)
            let reads = probe.windowOffsets.count
            let copied = await model.copyOriginalHex()
            precondition(copied?.utf8.count == 65536 * 2 && probe.windowOffsets.count == reads,
                "Copy must use the accepted observation without rereading the selected file")
            model.back()
            try await until { !model.busy && model.page?.visibleRange.start == 0 }
            precondition(probe.windowOffsets.count == reads && probe.restoreCount == 1)

            let owner = model.info!.identity.owner
            model.reopenCurrentFile()
            try await until { !model.busy && model.info?.identity.owner != owner }
            precondition(probe.openCount == 2 && probe.windowOffsets == [0, offset, 0]
                && probe.openedInputs.count == 2 && probe.openedInputs[0] == probe.openedInputs[1],
                "Explicit reopen must preserve exact-file input and start a new observation owner")
        }
        intake.close()
        try await until { probe.closedCount == probe.createCount && probe.hasReleased("large") }
    }
    static func failedFirstRangePreservesBothReaderKinds() async throws {
        let probe = ObservedReaderProbe(), intake = make(probe)
        open(intake, "small")
        try await until { intake.reader != nil && !intake.isLoading }
        let retained = intake.reader!, originalPresentation = intake.presentationID!
        probe.failWindow("OBSERVED_READER_IO")
        open(intake, "first-failed")
        try await until { !intake.isLoading }
        precondition(intake.reader === retained && intake.presentationID == originalPresentation
            && !retained.closed && intake.notice.contains("unchanged") && !probe.hasReleased("small"),
            "A failed first observation must preserve the accepted complete reader and its grant")
        try await until { probe.closedCount == 1 && probe.hasReleased("first-failed") }

        open(intake, "range-good")
        try await until { intake.reader?.presentation != nil && !intake.isLoading }
        precondition(retained.closed)
        do {
            let observed = intake.reader!, page = observed.presentation!.model.page!
            let observedPresentation = intake.presentationID!
            intake.dismissReader(originalPresentation)
            precondition(intake.reader === observed && !observed.closed && observed.presentation?.model.page != nil,
                "A disappearing old sheet must not retire the newly accepted range reader")
            probe.failWindow("OBSERVED_READER_IO")
            open(intake, "second-failed")
            try await until { !intake.isLoading }
            precondition(intake.reader === observed && intake.presentationID == observedPresentation
                && observed.presentation?.model.page?.matches(page) == true && !observed.closed,
                "A failed replacement must preserve an accepted range's observation and owner")
        }
        intake.close()
        try await until { probe.closedCount == probe.createCount && probe.hasReleased("range-good")
            && probe.hasReleased("second-failed") && probe.hasReleased("small") }
    }
    static func canceledFirstRangeDrainsBeforeNewestChoice() async throws {
        let probe = ObservedReaderProbe(), intake = make(probe), gate = ObservedReaderGate()
        open(intake, "small")
        try await until { intake.reader != nil && !intake.isLoading }
        probe.block(0, on: gate)
        open(intake, "blocked")
        try await until { gate.entered }
        precondition(intake.reader?.presentation == nil && !probe.hasReleased("blocked"))
        open(intake, "superseded")
        open(intake, "latest")
        precondition(probe.createCount == 1 && probe.openCount == 1 && probe.activeReads == 1,
            "New choices cannot create more readers while the first entered range is draining")
        gate.release()
        try await until { !intake.isLoading && intake.fileURL?.lastPathComponent == "latest" }
        precondition(probe.maximumActive == 1 && probe.createCount == 2 && probe.openCount == 2
            && probe.openedInputs.map(\.displayPath) == ["/selection/blocked", "/selection/latest"],
            "Only the newest queued choice may publish after cancellation during the first range")
        try await until { probe.hasReleased("blocked") && probe.hasReleased("superseded") }
        intake.close()
        try await until { probe.closedCount == probe.createCount && probe.hasReleased("latest") }
    }
    static func closeDuringFirstRange() async throws {
        let probe = ObservedReaderProbe(), intake = make(probe), gate = ObservedReaderGate()
        probe.block(0, on: gate)
        open(intake, "closed")
        try await until { gate.entered }
        intake.close()
        precondition(intake.reader == nil && !intake.showsReader && !intake.canReceive
            && !probe.hasReleased("closed") && probe.activeReads == 1,
            "Closing revokes publication immediately but keeps the entered file grant alive")
        gate.release()
        try await until { probe.closedCount == 1 && probe.hasReleased("closed") }
        precondition(intake.reader == nil && intake.presentationID == nil && !intake.showsReader,
            "A valid late first-page receipt must never recreate the closed reader")
    }
}
