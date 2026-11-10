// The production Swift adapter calls these public-header-checked C fixtures.
// Scripted responses qualify marshaling and ownership only, not archive I/O.
import Foundation

extension AtlasReaderTransport {
    static let native = SnapshotComparisonFixture().readerTransport
}
@_silgen_name("fixture_snapshot_comparison_mode") private func mode(_ value: Int32)
@_silgen_name("fixture_snapshot_comparison_count") private func count(_ index: UInt64) -> UInt64

@main private enum AtlasSnapshotComparisonBridgeTests {
    static func main() throws {
        let transport = AtlasSnapshotComparisonTransport.native, flag = AtlasSearchCancellation()
        let owner = UInt64.max - 10, generation = UInt64.max - 9
        let before = "/before-e\u{301}.fcbs", after = "/after-é.fcbs"
        precondition(transport.create() == owner)
        precondition(value { try transport.open(owner, before, after, flag) } == "{}")
        precondition(value { try transport.info(owner) } == "{}")
        precondition(value { try transport.begin(owner, generation, flag) } == "{}")
        precondition(value { try transport.step(owner, generation, 256, flag) } == "{}")
        for filter in AtlasSnapshotComparisonFilter.allCases {
            precondition(value { try transport.page(owner, generation, filter, UInt64.max - 7, 64) } == "{}")
        }
        for side in AtlasSnapshotComparisonSide.allCases {
            precondition(value { try transport.openReader(owner, UInt64.max - 6, generation, UInt64.max - 5, side) } == "{}")
        }
        precondition(transport.cancel(owner) && transport.close(owner))
        precondition(count(0) == 1 && count(1) == 1 && count(2) == 1 && count(3) == 1 && count(4) == 1
            && count(5) == 9 && count(6) == 2 && count(7) == 1 && count(8) == 1 && count(9) == 15)
        for invalid in ["relative.fcbs", "/a\0.fcbs", "/" + String(repeating: "x", count: 4096)] {
            do { _ = try transport.open(owner, invalid, after, flag); preconditionFailure("Invalid native archive path crossed ABI") }
            catch AtlasSnapshotComparisonError.invalidInput {}
        }
        precondition(count(1) == 1)
        mode(1)
        precondition(value { try transport.info(owner) } == nil && count(9) == 15)
        for invalid in [Int32(2), 3, 4] {
            mode(invalid)
            do { _ = try transport.info(owner); preconditionFailure("Malformed C response was admitted") }
            catch AtlasSnapshotComparisonError.invalidResponse {}
        }
        precondition(count(9) == 18, "Every non-null rejected C buffer is released exactly once")
        flag.cancel(); mode(5)
        _ = try transport.open(owner, before, after, flag)
        _ = try transport.begin(owner, generation, flag)
        _ = try transport.step(owner, generation, 256, flag)
        precondition(count(9) == 21, "All cancelable calls borrow the exact request flag through return")
        print("AtlasSnapshotComparisonBridgeTests: native widths, path bytes, filters, poll lifetimes and buffer ownership passed")
    }
    private static func value(_ body: () throws -> String?) -> String? { try! body() }
}
