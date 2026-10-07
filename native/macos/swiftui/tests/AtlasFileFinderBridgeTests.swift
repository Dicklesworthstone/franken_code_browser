import Foundation

@_silgen_name("file_fixture_count") private func fixtureCount(_ key: UInt32) -> UInt32
@_silgen_name("file_fixture_bad") private func fixtureBad(_ kind: Int32)
@main private enum AtlasFileFinderBridgeTests {
    static func main() throws {
        var checks = 0
        func check(_ value: Bool, _ message: String) { precondition(value, message); checks += 1 }
        let transport = AtlasFileFinderTransport.native, handle = transport.create(), generation = UInt64.max - 1
        check(handle == 9_007_199_254_740_993, "Full-width native handles remain integers")
        let flag = AtlasSearchCancellation()
        check(transport.open(handle, "/repo/é", 20_000, flag) == "opened", "Root UTF-8 and cancellation context reach C")
        for mode in AtlasFileFindMode.allCases {
            for sensitive in [false, true] {
                let query = try AtlasFileQuery(text: "é.rs", mode: mode, matchCase: sensitive)
                check(transport.find(handle, generation, query, 256, flag) == mode.wire, "Explicit mode/case arguments pass through")
            }
        }
        check(transport.page(handle, generation, 64, 128) == "page", "Paging preserves offsets and generation")
        check(transport.select(handle, generation, 255) == "selected", "Selection uses file ID, not a row index")
        for kind: Int32 in [1, 2, 3] {
            fixtureBad(kind)
            check(transport.page(handle, generation, 64, 128) == nil, "Invalid UTF-8, oversized and absent C responses are refused")
        }
        fixtureBad(0)
        flag.cancel()
        check(transport.open(handle, "/repo/é", 20_000, flag) == nil, "Open callback observes cancellation")
        check(transport.find(handle, generation, try AtlasFileQuery(text: "é.rs"), 256, flag) == nil,
              "Find callback observes cancellation without borrowing input lifetime")
        check(transport.close(handle), "Native owner is explicitly retired")
        check(fixtureCount(6) == fixtureCount(7), "Every non-null C allocation is freed, including rejected responses")
        print("AtlasFileFinderBridgeTests: \(checks) checks passed")
    }
}
