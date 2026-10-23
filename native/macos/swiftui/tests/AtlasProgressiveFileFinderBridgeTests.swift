import Foundation

@_silgen_name("progressive_fixture_bad") private func fixtureBad(_ value: Int32)
@_silgen_name("progressive_fixture_allocations") private func fixtureAllocations() -> UInt32
@_silgen_name("progressive_fixture_frees") private func fixtureFrees() -> UInt32

// Only the reader-domain lookup is needed by this finder marshaling fixture.
// Real retained-reader ownership is exercised by AtlasProgressiveFileFinderTests.
extension AtlasReaderTransport {
    static let native = Self(create: { 0 }, open: { _, _, _ in nil },
        window: { _, _, _ in nil }, lines: { _, _, _, _ in nil },
        cancel: { _ in false }, close: { _ in false }, retirementFailed: { })
}

@main private enum AtlasProgressiveFileFinderBridgeTests {
    static func main() throws {
        let transport = AtlasProgressiveFileTransport.native
        let handle = transport.create(), generation = UInt64.max - 1, revision: UInt64 = 9_007_199_254_740_995
        precondition(handle == 9_007_199_254_740_993)
        for mode in AtlasFileFindMode.allCases {
            for sensitive in [false, true] {
                let query = try AtlasFileQuery(text: "é.rs", mode: mode, matchCase: sensitive)
                let expected = mode.wire + (sensitive ? "-sensitive" : "-lowercase")
                precondition(transport.run(handle, .open(root: "/repo/é", generation: generation, query: query)) == expected)
            }
        }
        precondition(transport.run(handle, .info) == "info")
        precondition(transport.run(handle, .page(generation: generation, revision: revision)) == "page")
        precondition(transport.run(handle, .advance(generation: generation, after: revision, batches: 16)) == "advance")
        precondition(transport.run(handle, .stop(generation: generation)) == "stop")
        precondition(transport.run(handle, .openReader(reader: 999, generation: generation,
            revision: revision, file: 20_001, bytes: AtlasReaderLimits.captureBytes)) == "reader")
        for bad: Int32 in [1, 2, 3] {
            fixtureBad(bad)
            precondition(transport.run(handle, .info) == nil)
        }
        fixtureBad(0)
        precondition(transport.cancel(handle) && transport.close(handle))
        precondition(fixtureAllocations() == fixtureFrees())
        print("Progressive filename ABI mode/case/raw-query/u64/reader/response-ownership regressions passed.")
    }
}
