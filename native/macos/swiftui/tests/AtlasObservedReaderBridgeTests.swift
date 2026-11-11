import Foundation

@_silgen_name("observed_reader_fixture_reset") private func reset()
@_silgen_name("observed_reader_fixture_mode") private func mode(_ value: Int32)
@_silgen_name("observed_reader_fixture_count") private func count(_ kind: UInt32) -> UInt32
@_silgen_name("observed_reader_fixture_allocations") private func allocations() -> UInt32
@_silgen_name("observed_reader_fixture_frees") private func frees() -> UInt32
@_silgen_name("observed_reader_fixture_encodings") private func encodings() -> UInt32

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
                range: try .init(start: offset + 1024, end: offset + 1024 + UInt64(searchRequest.needle.utf8.count))))
        let witness = AtlasObservedReaderSearchWitness(target: target, encoding: .utf8,
            text: searchRequest.needle, originalHex: searchRequest.originalHex, sha256: String(repeating: "a", count: 64))
        let verifiedHit = try observeHit(handle, witness, request, AtlasSearchCancellation())
        precondition(verifiedHit == expected && count(11) == 1 && allocations() == 12 && frees() == 12)
        let legacy = AtlasObservedReaderSearchTransport(find: search.find, hit: search.hit)
        precondition(legacy.observeHit == nil, "Existing search-only transports retain their optional capability boundary")
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

        // Every allocating entrypoint shares the actual bounded take/free path.
        let commands: [() throws -> String?] = [
            { try transport.open(handle, input, AtlasSearchCancellation()) },
            { try transport.info(handle) },
            { try transport.window(handle, request, AtlasSearchCancellation()) },
            { try transport.retained(handle, observation) },
            { try transport.copy(handle, observation, offset + 3, offset + 17) },
            { try search.find(handle, searchRequest, AtlasSearchCancellation()) },
            { try search.hit(handle, observation, 255) },
            { try observeHit(handle, witness, request, AtlasSearchCancellation()) },
            { try readLines(handle, lineRequest, AtlasSearchCancellation()) },
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
        for operation in 0..<5 {
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
                        default: _ = try readLines(handle, lineRequest, token)
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
        let enteredOpen = count(1), enteredWindow = count(3), enteredPolls = count(8), enteredFind = count(9)
        let enteredObserveHit = count(11)
        let enteredLines = count(12)
        do { _ = try transport.open(handle, input, canceled); preconditionFailure("Pre-entry open cancellation ignored") }
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
        precondition(transport.cancel(handle) && transport.close(handle))
        mode(6)
        precondition(transport.create() == 0 && !transport.cancel(handle) && !transport.close(handle))
        precondition(count(0) == 2 && count(6) == 2 && count(7) == 2)
        precondition(allocations() == frees())
        print("Observed reader production bridge ABI, callback lifetime, cancellation and exact allocation cleanup fixtures passed.")
    }
}
