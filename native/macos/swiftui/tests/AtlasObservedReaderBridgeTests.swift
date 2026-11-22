import Foundation

@_silgen_name("observed_reader_fixture_reset") private func reset()
@_silgen_name("observed_reader_fixture_mode") private func mode(_ value: Int32)
@_silgen_name("observed_reader_fixture_count") private func count(_ kind: UInt32) -> UInt32
@_silgen_name("observed_reader_fixture_allocations") private func allocations() -> UInt32
@_silgen_name("observed_reader_fixture_frees") private func frees() -> UInt32
@_silgen_name("observed_reader_fixture_encodings") private func encodings() -> UInt32
@_silgen_name("observed_reader_fixture_position_kinds") private func positionKinds() -> UInt32

@_cdecl("observed_reader_fixture_cancel") func cancelObservedReaderFixture(_ context: UnsafeMutableRawPointer?) {
    guard let context else { preconditionFailure("Missing live cancellation context") }
    Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().cancel()
}

/// Links the production transport to a C ABI injection, with no replacement
/// Swift bridge or decoder. Range receipt semantics are tested separately.
@main private enum AtlasObservedReaderBridgeTests {
    static func main() throws {
        reset()
        let transport = AtlasObservedReaderTransport.native
        let handle: UInt64 = 0xfedcba9876543210
        let offset: UInt64 = 0x0020000100000047
        let observation: UInt64 = 0x8000000100000007
        let expected = "{\"bridge_fixture\":\"observed-range\"}"
        let input = try AtlasObservedReaderInput(root: "/repo-é",
            relativePath: [0x73, 0x72, 0x63, 0x2f, 0xff, 0x5c, 0x22, 0x0a, 0x2e, 0x72, 0x73])
        let request = try AtlasObservedReaderRequest(offset: offset, encoding: .utf8)
        precondition(transport.create() == handle && count(0) == 1,
            "Create must return every bit of its UInt64 handle")
        let first = try transport.open(handle, input, AtlasSearchCancellation())
        precondition(first == expected && count(1) == 1 && count(8) == 1)
        for encoding in AtlasObservedReaderEncoding.allCases {
            let selected = try AtlasObservedReaderRequest(offset: offset, encoding: encoding)
            let page = try transport.window(handle, selected, AtlasSearchCancellation())
            precondition(page == expected)
        }
        precondition(encodings() == 0b1110 && count(3) == 3,
            "UTF-8/UTF-16LE/UTF-16BE must use the public 1/2/3 ABI codes")
        let info = try transport.info(handle)
        let retained = try transport.retained(handle, observation)
        let copied = try transport.copy(handle, observation, offset + 3, offset + 17)
        precondition(info == expected && retained == expected && copied == expected)
        precondition(count(2) == 1 && count(4) == 1 && count(5) == 1)
        precondition(allocations() == 7 && frees() == 7)
        guard let search = transport.search else { preconditionFailure("Native Find transport is missing") }
        let searchRequest = try AtlasObservedReaderSearchRequest(generation: observation,
            needle: "exact é😀", encoding: .utf8, maximumScanBytes: 1 << 32)
        for encoding in AtlasObservedReaderEncoding.allCases {
            let request = try AtlasObservedReaderSearchRequest(generation: observation,
                needle: "exact é😀", encoding: encoding, maximumScanBytes: 1 << 32)
            let result = try search.find(handle, request, AtlasSearchCancellation())
            precondition(result == expected)
        }
        let hit = try search.hit(handle, observation, 255)
        precondition(hit == expected && count(9) == 3 && count(10) == 1)
        precondition(allocations() == 11 && frees() == 11)
        guard let observeHit = search.observeHit else { preconditionFailure("Native verified-hit transport is missing") }
        let target = AtlasObservedReaderSearchTarget(identity: .init(owner: handle, file: 1,
            path: input.absolutePath, displayPath: "fixture"), generation: observation, observation: 8,
            hit: .init(index: 255, occurrence: 256,
                range: try .init(start: offset + 1024, end: offset + 1024 + UInt64(searchRequest.needle.utf8.count)),
                witnessSHA256: String(repeating: "a", count: 64)))
        let witness = AtlasObservedReaderSearchWitness(target: target, encoding: .utf8,
            text: searchRequest.needle, originalHex: searchRequest.originalHex, sha256: String(repeating: "a", count: 64))
        let verifiedHit = try observeHit(handle, witness, request, AtlasSearchCancellation())
        precondition(verifiedHit == expected && count(11) == 1 && allocations() == 12 && frees() == 12)
        let legacy = AtlasObservedReaderSearchTransport(find: search.find, hit: search.hit)
        precondition(legacy.observeHit == nil && legacy.findPage == nil && legacy.openExcerpt == nil,
            "Existing search-only transports retain their optional capability boundary")
        guard let readLines = transport.lines else { preconditionFailure("Native physical-line transport is missing") }
        let lineRequest = try AtlasObservedReaderLineRequest(firstLine: offset, lines: 4096,
            maximumScanBytes: 1 << 32)
        for encoding in AtlasObservedReaderEncoding.allCases {
            let selected = try AtlasObservedReaderLineRequest(firstLine: offset, lines: 4096,
                maximumScanBytes: 1 << 32, encoding: encoding)
            let result = try readLines(handle, selected, AtlasSearchCancellation())
            precondition(result == expected)
        }
        precondition(count(12) == 3 && allocations() == 15 && frees() == 15,
            "Line requests preserve UInt64 coordinates, independent byte limits and explicit decoding at the ABI")

        let selectedFile = try AtlasObservedReaderInput(selectedFile: input.absolutePath)
        let projectOpens = count(1), priorPolls = count(8)
        let selectedFirst = try transport.open(handle, selectedFile, AtlasSearchCancellation())
        precondition(selectedFirst == expected && selectedFile != input && count(19) == 1
            && count(1) == projectOpens && count(8) == priorPolls + 1 && allocations() == frees(),
            "The same raw path selected as one file must use the exact-file ABI, without a parent project open")

        guard let resolveSelection = transport.resolveSelection else { preconditionFailure("Native selection transport is missing") }
        let occurrences = try AtlasObservedReaderSearchRequest(generation: observation,
            needle: "exact_é", encoding: .utf8, maximumScanBytes: 1 << 32, mode: .occurrences)
        guard let findPage = search.findPage else { preconditionFailure("Native result-page transport is missing") }
        let pageRequest = try AtlasObservedReaderSearchRequest(generation: observation,
            needle: "exact é😀", encoding: .utf8, maximumScanBytes: 1 << 32, pageStartAfter: offset)

        // Inconsistent requests must not cross the ABI or let the host silently
        // reduce their prefix and return a different requested observation.
        let invalidHitRequests = [
            try AtlasObservedReaderRequest(offset: offset, encoding: .utf16be),
            try AtlasObservedReaderRequest(offset: target.hit.range.start + 1),
            try AtlasObservedReaderRequest(offset: offset, bytes: 4),
            try AtlasObservedReaderRequest(offset: target.hit.range.start - 65536),
        ]
        let beforeInvalid = count(11), beforeInvalidAllocations = allocations()
        for invalid in invalidHitRequests {
            do {
                _ = try observeHit(handle, witness, invalid, AtlasSearchCancellation())
                preconditionFailure("Inconsistent verified-hit request reached the native ABI")
            } catch AtlasObservedReaderError.invalidInput { }
        }
        precondition(count(11) == beforeInvalid && allocations() == beforeInvalidAllocations)

        guard let openExcerpt = search.openExcerpt else { preconditionFailure("Native captured-excerpt transport is missing") }
        let capturedRange = try AtlasObservedByteRange(start: target.hit.range.start - 160, end: target.hit.range.end + 160)
        let capturedText = String(repeating: "x", count: 156) + witness.text + String(repeating: "x", count: 156)
        var capturedWitness = witness
        capturedWitness.context = .init(requestedRange: capturedRange, originalRange: capturedRange,
            complete: true, originalHex: String(repeating: "78", count: 160) + witness.originalHex + String(repeating: "78", count: 160),
            text: capturedText, textRange: try .init(start: capturedRange.start + 4, end: capturedRange.end - 4),
            matchedTextRange: try .init(start: 156, end: 156 + UInt64(witness.text.utf8.count)),
            hasReplacements: false, unavailableReason: nil)
        let beforeExcerpt = allocations()
        let openedExcerpt = try openExcerpt(handle, capturedWitness, AtlasSearchCancellation())
        precondition(openedExcerpt == expected)
        precondition(count(28) == 1 && allocations() == beforeExcerpt + 1 && allocations() == frees())
        do { _ = try openExcerpt(handle, witness, AtlasSearchCancellation()); preconditionFailure("Missing captured context crossed the ABI") }
        catch AtlasObservedReaderError.invalidInput { }
        precondition(count(28) == 1)

        // Every allocating entrypoint shares the actual bounded take/free path.
        let commands: [() throws -> String?] = [
            { try transport.open(handle, input, AtlasSearchCancellation()) },
            { try transport.open(handle, selectedFile, AtlasSearchCancellation()) },
            { try transport.info(handle) },
            { try transport.window(handle, request, AtlasSearchCancellation()) },
            { try transport.retained(handle, observation) },
            { try transport.copy(handle, observation, offset + 3, offset + 17) },
            { try search.find(handle, searchRequest, AtlasSearchCancellation()) },
            { try search.hit(handle, observation, 255) },
            { try observeHit(handle, witness, request, AtlasSearchCancellation()) },
            { try readLines(handle, lineRequest, AtlasSearchCancellation()) },
            { try resolveSelection(handle, observation, offset + 3, offset + 17) },
            { try search.find(handle, occurrences, AtlasSearchCancellation()) },
            { try findPage(handle, pageRequest, AtlasSearchCancellation()) },
            { try openExcerpt(handle, capturedWitness, AtlasSearchCancellation()) },
        ]
        for command in commands {
            mode(1)
            let beforeAllocations = allocations(), beforeFrees = frees()
            let absent = try command()
            precondition(absent == nil)
            precondition(allocations() == beforeAllocations && frees() == beforeFrees,
                "A null response is never passed to the library free function")
            for invalid: Int32 in [2, 3] {
                mode(invalid)
                let before = frees()
                do { _ = try command(); preconditionFailure("Invalid foreign UTF-8 or oversized response accepted") }
                catch AtlasObservedReaderError.invalidResponse { }
                precondition(frees() == before + 1 && allocations() == frees(),
                    "Rejected foreign allocations are freed exactly once")
            }
        }

        // C invokes the supplied callback with its live object context. The
        // transport frees the obsolete reply before throwing cancellation.
        for operation in 0..<9 {
            for lateMode: Int32 in [4, 5] {
                mode(lateMode)
                weak var released: AtlasSearchCancellation?
                let beforeAllocations = allocations(), beforeFrees = frees()
                do {
                    let token = AtlasSearchCancellation(); released = token
                    do {
                        switch operation {
                        case 0: _ = try transport.open(handle, input, token)
                        case 1: _ = try transport.window(handle, request, token)
                        case 2: _ = try search.find(handle, searchRequest, token)
                        case 3: _ = try observeHit(handle, witness, request, token)
                        case 4: _ = try readLines(handle, lineRequest, token)
                        case 5: _ = try search.find(handle, occurrences, token)
                        case 6: _ = try transport.open(handle, selectedFile, token)
                        case 7: _ = try findPage(handle, pageRequest, token)
                        default: _ = try openExcerpt(handle, capturedWitness, token)
                        }
                        preconditionFailure("Late canceled read-only delivery was not suppressed")
                    } catch AtlasObservedReaderError.canceled { }
                    precondition(token.isCanceled)
                }
                precondition(released == nil, "The synchronous bridge must not retain a callback context after return")
                let increment: UInt32 = lateMode == 4 ? 1 : 0
                precondition(allocations() == beforeAllocations + increment && frees() == beforeFrees + increment)
            }
        }

        mode(0)
        let countedRequest = try AtlasObservedReaderSearchRequest(generation: observation,
            needle: "exact é😀", encoding: .utf8, maximumScanBytes: 1 << 32, countAllMatches: true)
        let counted = try search.find(handle, countedRequest, AtlasSearchCancellation())
        precondition(counted == expected && count(13) == 1,
            "Counted Find must use the additive counted ABI without changing legacy Find dispatch")

        let canceled = AtlasSearchCancellation(); canceled.cancel()
        let exactEntries = count(9), countedEntries = count(13)
        for encoding in AtlasObservedReaderEncoding.allCases {
            for countAll in [false, true] {
                let lower = try AtlasObservedReaderSearchRequest(generation: observation, needle: searchRequest.needle,
                    encoding: encoding, maximumScanBytes: 1 << 32, countAllMatches: countAll, caseSensitive: false)
                let result = try search.find(handle, lower, AtlasSearchCancellation())
                precondition(result == expected && !lower.caseSensitive)
            }
        }
        precondition(count(14) == 6 && count(15) == 3 && count(9) == exactEntries && count(13) == countedEntries,
            "Lowercase Find freezes both policy flags and preserves the exact legacy dispatch")
        let lower = try AtlasObservedReaderSearchRequest(generation: observation, needle: searchRequest.needle,
            encoding: .utf8, maximumScanBytes: 1 << 32, countAllMatches: true, caseSensitive: false)
        let matchingEntries = count(14)
        do { _ = try search.find(handle, lower, canceled); preconditionFailure("Canceled lowercase Find crossed the ABI") }
        catch AtlasObservedReaderError.canceled { }
        precondition(count(14) == matchingEntries)
        for invalid in [Int32(2), Int32(3)] {
            mode(invalid); let released = frees()
            do { _ = try search.find(handle, lower, AtlasSearchCancellation()); preconditionFailure("Malformed lowercase response accepted") }
            catch AtlasObservedReaderError.invalidResponse { }
            precondition(frees() == released + 1 && allocations() == frees())
        }
        for late in [Int32(4), Int32(5)] {
            mode(late)
            weak var released: AtlasSearchCancellation?
            do {
                let token = AtlasSearchCancellation(); released = token
                do { _ = try search.find(handle, lower, token); preconditionFailure("Late lowercase cancellation accepted") }
                catch AtlasObservedReaderError.canceled { }
                precondition(token.isCanceled)
            }
            precondition(released == nil && allocations() == frees())
        }
        mode(0)
        let enteredOpen = count(1), enteredWindow = count(3), enteredPolls = count(8), enteredFind = count(9)
        let enteredObserveHit = count(11)
        let enteredLines = count(12)
        let enteredFile = count(19)
        do { _ = try transport.open(handle, input, canceled); preconditionFailure("Pre-entry open cancellation ignored") }
        catch AtlasObservedReaderError.canceled { }
        do { _ = try transport.open(handle, selectedFile, canceled); preconditionFailure("Pre-entry exact-file cancellation ignored") }
        catch AtlasObservedReaderError.canceled { }
        do { _ = try transport.window(handle, request, canceled); preconditionFailure("Pre-entry window cancellation ignored") }
        catch AtlasObservedReaderError.canceled { }
        do { _ = try search.find(handle, searchRequest, canceled); preconditionFailure("Pre-entry Find cancellation ignored") }
        catch AtlasObservedReaderError.canceled { }
        do { _ = try observeHit(handle, witness, request, canceled); preconditionFailure("Pre-entry verified-hit cancellation ignored") }
        catch AtlasObservedReaderError.canceled { }
        do { _ = try readLines(handle, lineRequest, canceled); preconditionFailure("Pre-entry line scan cancellation ignored") }
        catch AtlasObservedReaderError.canceled { }
        precondition(count(1) == enteredOpen && count(3) == enteredWindow && count(8) == enteredPolls && count(9) == enteredFind)
        precondition(count(11) == enteredObserveHit)
        precondition(count(12) == enteredLines)
        precondition(count(19) == enteredFile)
        let beforeOccurrences = count(16), beforeCountedOccurrences = count(17)
        let legacyFinds = count(9), legacyCounted = count(13), legacyMatching = count(14)
        for encoding in AtlasObservedReaderEncoding.allCases {
            for countAll in [false, true] {
                let selectedMode = try AtlasObservedReaderSearchRequest(generation: observation, needle: "exact_é",
                    encoding: encoding, maximumScanBytes: 1 << 32, countAllMatches: countAll, mode: .occurrences)
                let result = try search.find(handle, selectedMode, AtlasSearchCancellation())
                precondition(result == expected)
            }
        }
        precondition(count(16) == beforeOccurrences + 6 && count(17) == beforeCountedOccurrences + 3)
        precondition(count(9) == legacyFinds && count(13) == legacyCounted && count(14) == legacyMatching,
            "Occurrences must use the additive mode ABI; literal dispatch remains unchanged")
        let enteredOccurrences = count(16)
        do { _ = try search.find(handle, occurrences, canceled); preconditionFailure("Canceled occurrences crossed ABI") }
        catch AtlasObservedReaderError.canceled { }
        precondition(count(16) == enteredOccurrences)
        let enteredPages = count(20)
        do { _ = try findPage(handle, pageRequest, canceled); preconditionFailure("Canceled page Find crossed ABI") }
        catch AtlasObservedReaderError.canceled { }
        precondition(count(20) == enteredPages)
        let beforePages = count(20), beforeCountedPages = count(21), beforeLowercasePages = count(22), beforeOccurrencePages = count(23)
        let beforeLegacyFinds = count(9), beforeLegacyCounted = count(13), beforeLegacyMatching = count(14), beforeLegacyOccurrences = count(16)
        for encoding in AtlasObservedReaderEncoding.allCases {
            for countAll in [false, true] {
                for (selectedMode, caseSensitive, needle) in [
                    (AtlasObservedReaderSearchMode.text, true, "exact é😀"),
                    (.text, false, "exact é😀"), (.occurrences, true, "exact_é"),
                ] {
                    let selectedPage = try AtlasObservedReaderSearchRequest(generation: observation, needle: needle,
                        encoding: encoding, maximumScanBytes: 1 << 32, countAllMatches: countAll,
                        caseSensitive: caseSensitive, mode: selectedMode, pageStartAfter: offset)
                    let result = try findPage(handle, selectedPage, AtlasSearchCancellation())
                    precondition(result == expected && allocations() == frees())
                }
            }
        }
        precondition(count(20) == beforePages + 18 && count(21) == beforeCountedPages + 9
            && count(22) == beforeLowercasePages + 6 && count(23) == beforeOccurrencePages + 6,
            "The page ABI preserves full-width skip positions, all decoding modes, matching policies and count flags")
        do {
            _ = try search.find(handle, pageRequest, AtlasSearchCancellation())
            preconditionFailure("Legacy Find silently ignored a nonzero page position")
        } catch AtlasObservedReaderError.invalidInput { }
        precondition(count(9) == beforeLegacyFinds && count(13) == beforeLegacyCounted
            && count(14) == beforeLegacyMatching && count(16) == beforeLegacyOccurrences,
            "Explicit page scans do not reuse a legacy ABI that would drop the requested skip")
        let expressionText = "\"exact é😀\" required -excluded"
        let expressionFinds = count(24), expressionCounts = count(25), expressionLower = count(26), expressionPages = count(27)
        for encoding in AtlasObservedReaderEncoding.allCases {
            for countAll in [false, true] {
                for caseSensitive in [false, true] {
                    let expression = try AtlasObservedReaderSearchRequest(generation: observation, needle: expressionText,
                        encoding: encoding, maximumScanBytes: 1 << 32, countAllMatches: countAll,
                        caseSensitive: caseSensitive, mode: .expression)
                    let found = try search.find(handle, expression, AtlasSearchCancellation())
                    precondition(found == expected)
                    let page = try AtlasObservedReaderSearchRequest(generation: observation, needle: expressionText,
                        encoding: encoding, maximumScanBytes: 1 << 32, countAllMatches: countAll,
                        caseSensitive: caseSensitive, mode: .expression, pageStartAfter: offset)
                    let paged = try findPage(handle, page, AtlasSearchCancellation())
                    precondition(paged == expected)
                    precondition(allocations() == frees())
                }
            }
        }
        precondition(count(24) == expressionFinds + 12 && count(25) == expressionCounts + 6
            && count(26) == expressionLower + 6 && count(27) == expressionPages + 12,
            "Expression mode 2 preserves the full raw query, decoding, case/count policies and full-width rescan offset")
        precondition(count(9) == beforeLegacyFinds && count(13) == beforeLegacyCounted
            && count(14) == beforeLegacyMatching && count(16) == beforeLegacyOccurrences,
            "Expression dispatch cannot fall through to legacy literal or identifier search")
        let expression = try AtlasObservedReaderSearchRequest(generation: observation, needle: expressionText,
            encoding: .utf16be, maximumScanBytes: 1 << 32, countAllMatches: true, caseSensitive: false, mode: .expression)
        let expressionPage = try AtlasObservedReaderSearchRequest(generation: observation, needle: expressionText,
            encoding: .utf16be, maximumScanBytes: 1 << 32, countAllMatches: true, caseSensitive: false,
            mode: .expression, pageStartAfter: offset)
        let enteredExpressions = count(24), enteredExpressionPages = count(27)
        do { _ = try search.find(handle, expression, canceled); preconditionFailure("Canceled expression crossed ABI") }
        catch AtlasObservedReaderError.canceled { }
        do { _ = try findPage(handle, expressionPage, canceled); preconditionFailure("Canceled expression page crossed ABI") }
        catch AtlasObservedReaderError.canceled { }
        precondition(count(24) == enteredExpressions && count(27) == enteredExpressionPages)
        for invalid in [Int32(2), Int32(3)] {
            mode(invalid)
            for page in [false, true] {
                let beforeFrees = frees()
                do {
                    if page { _ = try findPage(handle, expressionPage, AtlasSearchCancellation()) }
                    else { _ = try search.find(handle, expression, AtlasSearchCancellation()) }
                    preconditionFailure("Malformed expression response accepted")
                } catch AtlasObservedReaderError.invalidResponse { }
                precondition(frees() == beforeFrees + 1 && allocations() == frees())
            }
        }
        for late in [Int32(4), Int32(5)] {
            mode(late)
            for page in [false, true] {
                let token = AtlasSearchCancellation()
                do {
                    if page { _ = try findPage(handle, expressionPage, token) }
                    else { _ = try search.find(handle, expression, token) }
                    preconditionFailure("Late expression cancellation accepted")
                } catch AtlasObservedReaderError.canceled { }
                precondition(token.isCanceled && allocations() == frees())
            }
        }
        mode(0)
        let beforeSelection = count(18)
        let selected = try resolveSelection(handle, observation, offset + 3, offset + 17)
        precondition(selected == expected && count(18) == beforeSelection + 1 && allocations() == frees(),
            "Selection ABI preserves full-width observation and decoded coordinates and frees its receipt")
        guard let readPosition = transport.position else { preconditionFailure("Native typed-position transport is missing") }
        let position = try AtlasObservedReaderPositionRequest(line: offset, column: observation,
            maximumScanBytes: 1 << 32)
        let priorPositionAllocations = allocations(), priorPositionPolls = count(8)
        for encoding in AtlasObservedReaderEncoding.allCases {
            for kind in AtlasObservedReaderColumnKind.allCases {
                let request = try AtlasObservedReaderPositionRequest(line: offset, column: observation, kind: kind,
                    maximumScanBytes: 1 << 32, encoding: encoding)
                let result = try readPosition(handle, request, AtlasSearchCancellation())
                precondition(result == expected && allocations() == frees())
            }
        }
        precondition(count(29) == 9 && count(8) == priorPositionPolls + 9
            && allocations() == priorPositionAllocations + 9 && positionKinds() == 0b1110,
            "Position ABI preserves full-width line and column, typed 1/2/3 column units, every encoding and both byte limits")
        let enteredPositions = count(29), priorPositionFrees = frees()
        do { _ = try readPosition(handle, position, canceled); preconditionFailure("Canceled position crossed ABI") }
        catch AtlasObservedReaderError.canceled { }
        precondition(count(29) == enteredPositions && frees() == priorPositionFrees)
        mode(1)
        let beforeMissingPosition = allocations()
        let missingPosition = try readPosition(handle, position, AtlasSearchCancellation())
        precondition(missingPosition == nil && allocations() == beforeMissingPosition && allocations() == frees(),
            "A null position response stays unavailable without inventing owned storage")
        for invalid in [Int32(2), Int32(3)] {
            mode(invalid); let beforeFrees = frees()
            do { _ = try readPosition(handle, position, AtlasSearchCancellation()); preconditionFailure("Malformed position response accepted") }
            catch AtlasObservedReaderError.invalidResponse { }
            precondition(frees() == beforeFrees + 1 && allocations() == frees(),
                "Every owned malformed or oversized position response is freed exactly once")
        }
        for late in [Int32(4), Int32(5)] {
            mode(late); let flag = AtlasSearchCancellation(), beforePolls = count(8)
            do { _ = try readPosition(handle, position, flag); preconditionFailure("Late position cancellation accepted") }
            catch AtlasObservedReaderError.canceled { }
            precondition(flag.isCanceled && count(8) == beforePolls + 2 && allocations() == frees(),
                "Position polls retain their context and free any late response before reporting cancellation")
        }
        mode(0)
        precondition(transport.cancel(handle) && transport.close(handle))
        mode(6)
        precondition(transport.create() == 0 && !transport.cancel(handle) && !transport.close(handle))
        precondition(count(0) == 2 && count(6) == 2 && count(7) == 2)
        precondition(allocations() == frees())
        print("Observed reader production bridge matching/count flags, callback lifetime, cancellation and exact allocation cleanup fixtures passed.")
    }
}
