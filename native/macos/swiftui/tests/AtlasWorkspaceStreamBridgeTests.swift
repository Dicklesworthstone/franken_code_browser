import Foundation

@_silgen_name("workspace_stream_fixture_reset") private func reset()
@_silgen_name("workspace_stream_fixture_mode") private func mode(_ value: Int32)
@_silgen_name("workspace_stream_fixture_scope") private func scope(_ value: UInt32)
@_silgen_name("workspace_stream_fixture_count") private func count(_ kind: UInt32) -> UInt32
@_silgen_name("workspace_stream_fixture_allocations") private func allocations() -> UInt32
@_silgen_name("workspace_stream_fixture_frees") private func frees() -> UInt32
@_cdecl("workspace_stream_fixture_cancel") func cancelWorkspaceStreamFixture(_ context: UnsafeMutableRawPointer?) {
    guard let context else { preconditionFailure("Missing live cancellation callback context") }
    Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().cancel()
}

private final class WorkspaceProgressCopies: @unchecked Sendable {
    private let lock = NSLock()
    private var received: [String] = []
    func append(_ text: String) { lock.lock(); received.append(text); lock.unlock() }
    var values: [String] { lock.lock(); defer { lock.unlock() }; return received }
}

@main private enum AtlasWorkspaceStreamBridgeTests {
    static func main() throws {
        reset()
        let transport = AtlasWorkspaceStreamTransport.native
        let owner: UInt64 = 0xfedcba9876543210, generation: UInt64 = 0x8000000100000007
        let request = try AtlasWorkspaceStreamRequest(needle: "exact é😀", maximumScanBytes: 1 << 32)
        let expected = "{\"fixture\":\"workspace-stream\"}"
        precondition(transport.create() == owner && count(0) == 1)
        let opened = try transport.open(owner, "/repo-é", AtlasSearchCancellation())
        let searched = try transport.search(owner, request, AtlasSearchCancellation())
        let page = try transport.page(owner, generation, 224, 32)
        let hit = try transport.hit(owner, generation, 255)
        let cleared = try transport.clear(owner)
        precondition([opened, searched, page, hit, cleared].allSatisfy { $0 == expected })
        precondition(allocations() == 5 && frees() == 5 && count(8) == 2)

        // A full injected receipt traverses the real C adapter and production
        // decoder together; this is not a claim of live Rust execution.
        mode(7)
        let catalog = try AtlasWorkspaceStreamCatalog.decode(transport.open(owner, "/repo-é", AtlasSearchCancellation()), owner: owner)
        let first = try AtlasWorkspaceStreamPage.decode(transport.search(owner, request, AtlasSearchCancellation()),
            catalog: catalog, root: catalog.root, request: request)
        let report = try AtlasWorkspaceStreamReport(summary: first.summary, hits: first.hits)
        precondition(report.summary.identity.owner == owner && report.summary.identity.generation == generation)
        precondition(report.summary.complete && report.hits.count == 1 && report.hits[0].range.start == 32)
        let witness = try AtlasWorkspaceStreamWitness.decode(transport.hit(owner, generation, 0), report: report,
            root: catalog.root, target: report.target(for: report.hits[0]))
        precondition(witness.text == "exact é😀" && witness.originalHex == "657861637420c3a9f09f9880")
        precondition(allocations() == 8 && frees() == 8)

        let commands: [() throws -> String?] = [
            { try transport.open(owner, "/repo-é", AtlasSearchCancellation()) },
            { try transport.search(owner, request, AtlasSearchCancellation()) },
            { try transport.page(owner, generation, 224, 32) },
            { try transport.hit(owner, generation, 255) },
            { try transport.clear(owner) },
        ]
        for command in commands {
            mode(1)
            let before = allocations(), absent = try command()
            precondition(absent == nil && allocations() == before && allocations() == frees())
            for invalid in [Int32(2), Int32(3)] {
                mode(invalid)
                let released = frees()
                do { _ = try command(); preconditionFailure("Invalid UTF-8 or oversized bridge response accepted") }
                catch AtlasWorkspaceStreamError.invalidResponse { }
                precondition(frees() == released + 1 && allocations() == frees())
            }
        }

        for operation in 0..<2 {
            for lateMode in [Int32(4), Int32(5)] {
                mode(lateMode)
                weak var released: AtlasSearchCancellation?
                let before = allocations(), releasedBefore = frees()
                do {
                    let flag = AtlasSearchCancellation(); released = flag
                    do {
                        if operation == 0 { _ = try transport.open(owner, "/repo-é", flag) }
                        else { _ = try transport.search(owner, request, flag) }
                        preconditionFailure("Canceled callback delivery accepted")
                    } catch AtlasWorkspaceStreamError.canceled { }
                    precondition(flag.isCanceled)
                }
                precondition(released == nil)
                let increment: UInt32 = lateMode == 4 ? 1 : 0
                precondition(allocations() == before + increment && frees() == releasedBefore + increment)
            }
        }
        mode(0)
        let flag = AtlasSearchCancellation(); flag.cancel()
        let opens = count(1), searches = count(2), polls = count(8)
        do { _ = try transport.open(owner, "/repo-é", flag); preconditionFailure("Canceled open crossed ABI") }
        catch AtlasWorkspaceStreamError.canceled { }
        do { _ = try transport.search(owner, request, flag); preconditionFailure("Canceled search crossed ABI") }
        catch AtlasWorkspaceStreamError.canceled { }
        precondition(count(1) == opens && count(2) == searches && count(8) == polls)
        precondition(transport.cancel(owner) && transport.close(owner))
        mode(6)
        precondition(transport.create() == 0 && !transport.cancel(owner) && !transport.close(owner))
        precondition(allocations() == frees())
        guard let progressive = transport.progressiveSearch else {
            preconditionFailure("Native progressive transport is missing")
        }
        mode(0)
        let copied = WorkspaceProgressCopies()
        let stop = AtlasWorkspaceStreamStop(), active = AtlasSearchCancellation()
        let stoppedBefore = count(12), beforeProgress = allocations()
        let terminal = try progressive(owner, request, active, stop) { text in
            copied.append(text); stop.requestStop()
        }
        precondition(terminal == expected && !active.isCanceled && stop.isRequested)
        precondition(copied.values == ["{\"fixture\":\"workspace-progress\"}"])
        precondition(count(12) == stoppedBefore + 1)
        precondition(allocations() == beforeProgress + 1 && allocations() == frees())

        let alreadyStopped = AtlasWorkspaceStreamStop(); alreadyStopped.requestStop()
        let silent = WorkspaceProgressCopies(), callbacks = count(10)
        _ = try progressive(owner, request, AtlasSearchCancellation(), alreadyStopped) { silent.append($0) }
        precondition(silent.values.isEmpty && count(10) == callbacks && count(12) == stoppedBefore + 2)

        // Malformed borrowed ranges must never reach the sink, and still drain
        // and free the independently owned terminal response exactly once.
        for malformed in [Int32(8), Int32(9), Int32(10), Int32(11)] {
            mode(malformed)
            let rejected = WorkspaceProgressCopies(), releasedBefore = frees(), canceledBefore = count(13)
            do {
                _ = try progressive(owner, request, AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) {
                    rejected.append($0)
                }
                preconditionFailure("Invalid borrowed progress bytes accepted")
            } catch AtlasWorkspaceStreamError.invalidResponse { }
            precondition(rejected.values.isEmpty && frees() == releasedBefore + 1)
            precondition(count(13) == canceledBefore + 1 && allocations() == frees())
        }
        for invalidTerminal in [Int32(2), Int32(3)] {
            mode(invalidTerminal)
            let validProgress = WorkspaceProgressCopies(), releasedBefore = frees()
            do {
                _ = try progressive(owner, request, AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) {
                    validProgress.append($0)
                }
                preconditionFailure("Malformed terminal progressive response accepted")
            } catch AtlasWorkspaceStreamError.invalidResponse { }
            precondition(validProgress.values.count == 1 && frees() == releasedBefore + 1)
            precondition(allocations() == frees())
        }

        // A sink can cancel while C still owns the call. Cancellation must
        // suppress even a subsequently successful or absent terminal reply.
        for terminalMode in [Int32(0), Int32(1)] {
            mode(terminalMode)
            weak var released: AtlasSearchCancellation?
            let releasedBefore = frees(), canceledBefore = count(13)
            do {
                let live = AtlasSearchCancellation(); released = live
                do {
                    _ = try progressive(owner, request, live, AtlasWorkspaceStreamStop()) { _ in live.cancel() }
                    preconditionFailure("Late progress cancellation accepted")
                } catch AtlasWorkspaceStreamError.canceled { }
            }
            precondition(released == nil && count(13) == canceledBefore + 1)
            precondition(frees() == releasedBefore + (terminalMode == 0 ? 1 : 0))
            precondition(allocations() == frees())
        }
        mode(1)
        let missing = try progressive(owner, request, AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) { _ in }
        precondition(missing == nil && allocations() == frees())
        mode(0)
        let canceledBeforeEntry = AtlasSearchCancellation(); canceledBeforeEntry.cancel()
        let entries = count(9), progressCalls = count(10)
        do {
            _ = try progressive(owner, request, canceledBeforeEntry, AtlasWorkspaceStreamStop()) { _ in }
            preconditionFailure("Canceled progressive search crossed ABI")
        } catch AtlasWorkspaceStreamError.canceled { }
        precondition(count(9) == entries && count(10) == progressCalls && allocations() == frees())

        // Location and file-type predicates cross the ABI unchanged and keep
        // their borrowed buffers alive through every synchronous callback.
        let directory = try AtlasWorkspaceStreamRequest(needle: request.needle, maximumScanBytes: request.maximumScanBytes,
            fileScope: .init(extensions: ["SWIFT", "rs"], filenames: ["Cargo.toml"]),
            pathScope: .init(kind: .directory, path: "src/é"))
        let file = try AtlasWorkspaceStreamRequest(needle: request.needle, maximumScanBytes: request.maximumScanBytes,
            pathScope: .init(kind: .file, path: "src/main.rs"))
        let emptyTypes = try AtlasWorkspaceStreamRequest(needle: request.needle, maximumScanBytes: request.maximumScanBytes,
            fileScope: .init(extensions: []))
        var scopedRequests: [(UInt32, AtlasWorkspaceStreamRequest)] = [(1, directory), (2, file), (3, emptyTypes)]
        #if canImport(CryptoKit)
        scopedRequests.append((4, try .init(needle: request.needle, maximumScanBytes: request.maximumScanBytes,
            pathScope: .init(visiblePaths: [[0x73, 0x72, 0x63, 0x2f, 0xff, 0x2e, 0x72, 0x73]]))))
        #endif
        let scopedBefore = count(14), progressiveBefore = count(15)
        for (selection, scoped) in scopedRequests {
            scope(selection)
            let direct = try transport.search(owner, scoped, AtlasSearchCancellation())
            let stopped = AtlasWorkspaceStreamStop()
            let streamed = try progressive(owner, scoped, AtlasSearchCancellation(), stopped) { _ in stopped.requestStop() }
            precondition(direct == expected && streamed == expected && stopped.isRequested)
            precondition(allocations() == frees())
        }
        precondition(count(14) == scopedBefore + UInt32(scopedRequests.count)
            && count(15) == progressiveBefore + UInt32(scopedRequests.count))
        precondition(count(16) == 0 && count(17) == 0,
            "Default requests must continue to call the original bounded ABI")

        // Explicit full counting selects the additive ABI without changing the
        // limits, scopes, copied callback bytes, or terminal ownership rules.
        let boundedEntries = count(14), boundedProgress = count(15)
        scopedRequests.append((0, request))
        for (selection, scoped) in scopedRequests {
            scope(selection)
            let counted = try AtlasWorkspaceStreamRequest(needle: scoped.needle,
                maximumScanBytes: scoped.maximumScanBytes, maximumMatches: scoped.maximumMatches,
                maximumReadCalls: scoped.maximumReadCalls, fileScope: scoped.fileScope,
                pathScope: scoped.pathScope, countAllMatches: true)
            let direct = try transport.search(owner, counted, AtlasSearchCancellation())
            let kept = AtlasWorkspaceStreamStop(), countedCopies = WorkspaceProgressCopies()
            let streamed = try progressive(owner, counted, AtlasSearchCancellation(), kept) { text in
                countedCopies.append(text); kept.requestStop()
            }
            precondition(direct == expected && streamed == expected && kept.isRequested)
            precondition(countedCopies.values == ["{\"fixture\":\"workspace-progress\"}"])
            precondition(allocations() == frees())
        }
        precondition(count(16) == UInt32(scopedRequests.count) && count(17) == UInt32(scopedRequests.count))
        precondition(count(14) == boundedEntries && count(15) == boundedProgress)
        scope(0)
        let counted = try AtlasWorkspaceStreamRequest(needle: request.needle,
            maximumScanBytes: request.maximumScanBytes, countAllMatches: true)
        let countedEntries = count(16), countedProgress = count(17)
        do {
            _ = try transport.search(owner, counted, canceledBeforeEntry)
            preconditionFailure("Canceled counted request crossed ABI")
        } catch AtlasWorkspaceStreamError.canceled { }
        do {
            _ = try progressive(owner, counted, canceledBeforeEntry, AtlasWorkspaceStreamStop()) { _ in }
            preconditionFailure("Canceled counted progress crossed ABI")
        } catch AtlasWorkspaceStreamError.canceled { }
        precondition(count(16) == countedEntries && count(17) == countedProgress)
        for lateMode in [Int32(4), Int32(5)] {
            mode(lateMode)
            let live = AtlasSearchCancellation()
            do {
                _ = try transport.search(owner, counted, live)
                preconditionFailure("Counted fallback accepted late cancellation")
            } catch AtlasWorkspaceStreamError.canceled { }
            precondition(live.isCanceled && allocations() == frees())
        }
        mode(8)
        let rejected = WorkspaceProgressCopies()
        do {
            _ = try progressive(owner, counted, AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) { rejected.append($0) }
            preconditionFailure("Counted search accepted malformed borrowed progress")
        } catch AtlasWorkspaceStreamError.invalidResponse { }
        precondition(rejected.values.isEmpty && allocations() == frees())
        mode(0)
        let legacyScoped = count(14), legacyProgress = count(15), legacyCounted = count(16), legacyCountedProgress = count(17)
        for (selection, scoped) in scopedRequests {
            scope(selection)
            for countAll in [false, true] {
                let lower = try AtlasWorkspaceStreamRequest(needle: scoped.needle,
                    maximumScanBytes: scoped.maximumScanBytes, maximumMatches: scoped.maximumMatches,
                    maximumReadCalls: scoped.maximumReadCalls, fileScope: scoped.fileScope,
                    pathScope: scoped.pathScope, countAllMatches: countAll, caseSensitive: false)
                let direct = try transport.search(owner, lower, AtlasSearchCancellation())
                let stop = AtlasWorkspaceStreamStop(), copies = WorkspaceProgressCopies()
                let terminal = try progressive(owner, lower, AtlasSearchCancellation(), stop) {
                    copies.append($0); stop.requestStop()
                }
                precondition(direct == expected && terminal == expected && stop.isRequested)
                precondition(copies.values == ["{\"fixture\":\"workspace-progress\"}"] && allocations() == frees())
            }
        }
        precondition(count(18) == UInt32(scopedRequests.count * 2) && count(19) == UInt32(scopedRequests.count * 2)
            && count(20) == UInt32(scopedRequests.count * 2))
        precondition(count(14) == legacyScoped && count(15) == legacyProgress
            && count(16) == legacyCounted && count(17) == legacyCountedProgress,
            "Lowercase requests must select the additive policy ABI without changing exact-case routes")
        scope(0)
        let lower = try AtlasWorkspaceStreamRequest(needle: request.needle, maximumScanBytes: request.maximumScanBytes,
            countAllMatches: true, caseSensitive: false)
        let matchingEntries = count(18), matchingProgress = count(19)
        do { _ = try transport.search(owner, lower, canceledBeforeEntry); preconditionFailure("Canceled lowercase request entered C") }
        catch AtlasWorkspaceStreamError.canceled { }
        do { _ = try progressive(owner, lower, canceledBeforeEntry, AtlasWorkspaceStreamStop()) { _ in }
            preconditionFailure("Canceled lowercase progress entered C")
        } catch AtlasWorkspaceStreamError.canceled { }
        precondition(count(18) == matchingEntries && count(19) == matchingProgress)
        for late in [Int32(4), Int32(5)] {
            mode(late); let token = AtlasSearchCancellation()
            do { _ = try transport.search(owner, lower, token); preconditionFailure("Late lowercase reply accepted") }
            catch AtlasWorkspaceStreamError.canceled { }
            precondition(token.isCanceled && allocations() == frees())
        }
        mode(8)
        let lowerCopies = WorkspaceProgressCopies()
        do {
            _ = try progressive(owner, lower, AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) { lowerCopies.append($0) }
            preconditionFailure("Malformed lowercase progress accepted")
        } catch AtlasWorkspaceStreamError.invalidResponse { }
        precondition(lowerCopies.values.isEmpty && allocations() == frees())
        mode(0)

        // Whole-token requests have their own additive ABI. Both count policies
        // must preserve scope buffers, callback lifetime and exact Unicode bytes.
        let beforeOccurrences = (UInt32(14)...UInt32(20)).map { count($0) }
        precondition(count(21) == 0 && count(22) == 0 && count(23) == 0)
        for (selection, scoped) in scopedRequests {
            scope(selection)
            for countAll in [false, true] {
                let occurrences = try AtlasWorkspaceStreamRequest(needle: "token_é😀",
                    maximumScanBytes: scoped.maximumScanBytes, maximumMatches: scoped.maximumMatches,
                    maximumReadCalls: scoped.maximumReadCalls, fileScope: scoped.fileScope,
                    pathScope: scoped.pathScope, countAllMatches: countAll, mode: .occurrences)
                precondition(occurrences.caseSensitive)
                let direct = try transport.search(owner, occurrences, AtlasSearchCancellation())
                let stop = AtlasWorkspaceStreamStop(), copies = WorkspaceProgressCopies()
                let terminal = try progressive(owner, occurrences, AtlasSearchCancellation(), stop) {
                    copies.append($0); stop.requestStop()
                }
                precondition(direct == expected && terminal == expected && stop.isRequested)
                precondition(copies.values == ["{\"fixture\":\"workspace-progress\"}"])
                precondition(allocations() == frees())
            }
        }
        precondition(count(21) == UInt32(scopedRequests.count * 2)
            && count(22) == UInt32(scopedRequests.count * 2)
            && count(23) == UInt32(scopedRequests.count * 2))
        precondition((UInt32(14)...UInt32(20)).map { count($0) } == beforeOccurrences,
            "Whole-token mode must never dispatch to literal or lowercase search")
        scope(0)
        let occurrences = try AtlasWorkspaceStreamRequest(needle: "token_é😀",
            maximumScanBytes: request.maximumScanBytes, countAllMatches: true, mode: .occurrences)
        let occurrenceEntries = count(21), occurrenceProgress = count(22), occurrenceCallbacks = count(10)
        do {
            _ = try transport.search(owner, occurrences, canceledBeforeEntry)
            preconditionFailure("Canceled occurrence request entered C")
        } catch AtlasWorkspaceStreamError.canceled { }
        do {
            _ = try progressive(owner, occurrences, canceledBeforeEntry, AtlasWorkspaceStreamStop()) { _ in }
            preconditionFailure("Canceled occurrence progress entered C")
        } catch AtlasWorkspaceStreamError.canceled { }
        precondition(count(21) == occurrenceEntries && count(22) == occurrenceProgress
            && count(10) == occurrenceCallbacks && allocations() == frees())

        let occurrenceStop = AtlasWorkspaceStreamStop(); occurrenceStop.requestStop()
        let noProgress = WorkspaceProgressCopies(), previousStops = count(12)
        let stoppedOccurrences = try progressive(owner, occurrences, AtlasSearchCancellation(), occurrenceStop) {
            noProgress.append($0)
        }
        precondition(stoppedOccurrences == expected && noProgress.values.isEmpty
            && count(10) == occurrenceCallbacks && count(12) == previousStops + 1)
        precondition(allocations() == frees())

        for late in [Int32(4), Int32(5)] {
            mode(late)
            let live = AtlasSearchCancellation(), releasedBefore = frees()
            do {
                _ = try transport.search(owner, occurrences, live)
                preconditionFailure("Occurrence terminal route accepted late cancellation")
            } catch AtlasWorkspaceStreamError.canceled { }
            precondition(live.isCanceled && frees() == releasedBefore + (late == 4 ? 1 : 0))
            precondition(allocations() == frees())
        }
        for terminalMode in [Int32(0), Int32(1)] {
            mode(terminalMode)
            weak var released: AtlasSearchCancellation?
            let releasedBefore = frees(), canceledBefore = count(13)
            do {
                let live = AtlasSearchCancellation(); released = live
                do {
                    _ = try progressive(owner, occurrences, live, AtlasWorkspaceStreamStop()) { _ in live.cancel() }
                    preconditionFailure("Occurrence callback cancellation accepted a terminal response")
                } catch AtlasWorkspaceStreamError.canceled { }
            }
            precondition(released == nil && count(13) == canceledBefore + 1)
            precondition(frees() == releasedBefore + (terminalMode == 0 ? 1 : 0))
            precondition(allocations() == frees())
        }

        for malformed in [Int32(8), Int32(9), Int32(10), Int32(11)] {
            mode(malformed)
            let copies = WorkspaceProgressCopies(), releasedBefore = frees(), canceledBefore = count(13)
            do {
                _ = try progressive(owner, occurrences, AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) {
                    copies.append($0)
                }
                preconditionFailure("Malformed occurrence progress reached the sink")
            } catch AtlasWorkspaceStreamError.invalidResponse { }
            precondition(copies.values.isEmpty && count(13) == canceledBefore + 1)
            precondition(frees() == releasedBefore + 1 && allocations() == frees())
        }
        for invalidTerminal in [Int32(2), Int32(3)] {
            mode(invalidTerminal)
            let copies = WorkspaceProgressCopies(), releasedBefore = frees()
            do {
                _ = try progressive(owner, occurrences, AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) {
                    copies.append($0)
                }
                preconditionFailure("Malformed occurrence terminal response accepted")
            } catch AtlasWorkspaceStreamError.invalidResponse { }
            precondition(copies.values == ["{\"fixture\":\"workspace-progress\"}"])
            precondition(frees() == releasedBefore + 1 && allocations() == frees())
        }
        mode(1)
        let absentOccurrences = try transport.search(owner, occurrences, AtlasSearchCancellation())
        let absentProgress = try progressive(owner, occurrences, AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) { _ in }
        precondition(absentOccurrences == nil && absentProgress == nil && allocations() == frees())
        mode(0)
        print("Workspace stream ABI occurrence/matching/count flags, ownership, progressive bytes, Stop and cancellation fixtures passed")
    }
}
