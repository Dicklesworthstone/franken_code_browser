// Tests the real Swift marshaling against header-checked C callbacks. These
// inert index/reader transports supply only dependencies of .native creation;
// no Rust graph, actual capture, native window or hardware result is implied.
import Foundation

extension AtlasIndexedSearchTransport {
    static let native = ProjectLinksFixture().index.transport
}
extension AtlasReaderTransport {
    static let native = ProjectLinksFixture().readerTransport
}
@_silgen_name("fixture_links_mode") private func mode(_ mode: Int32)
@_silgen_name("fixture_links_count") private func count(_ index: UInt64) -> UInt64

@main private enum AtlasProjectLinksBridgeTests {
    static func main() throws {
        let transport = AtlasProjectLinksTransport.native
        let owner = UInt64.max - 10, generation = UInt64.max - 9
        for (projection, base) in [(AtlasProjectRelationshipProjection.documentation, UInt64(0)), (.imports, UInt64(5))] {
            let operations = try transport.operations(for: projection), flag = AtlasSearchCancellation()
            let released = count(4)
            mode(0)
            precondition(tryValue { try operations.begin(owner, generation, UInt64.max - 8, flag) } == "{}")
            precondition(tryValue { try operations.step(owner, generation, flag) } == "{}")
            precondition(tryValue { try operations.page(owner, generation, .all, nil, UInt64.max - 7, 64) } == "{}")
            precondition(tryValue { try operations.page(owner, generation, .incoming, "ff2e7273", UInt64.max - 7, 64) } == "{}")
            for side in [AtlasProjectLinkSide.evidence, .target] {
                precondition(tryValue { try operations.openReader(owner, UInt64.max - 6, generation, UInt64.max - 5, side) } == "{}")
            }
            precondition(count(base) == 1 && count(base + 1) == 1 && count(base + 2) == 2
                && count(base + 3) == 2 && count(4) == released + 6)
            mode(1)
            precondition(tryValue { try operations.step(owner, generation, flag) } == nil && count(4) == released + 6)
            for invalid in [Int32(2), 3, 4] {
                mode(invalid)
                do { _ = try operations.step(owner, generation, flag); preconditionFailure("Malformed or oversized response was accepted") }
                catch AtlasProjectLinksError.invalidResponse {}
            }
            precondition(count(4) == released + 9, "Each non-null rejected C buffer is released once")
            flag.cancel(); mode(5)
            _ = try operations.begin(owner, generation, UInt64.max - 8, flag)
            _ = try operations.step(owner, generation, flag)
            precondition(count(4) == released + 11, "Cancellation callback observes the exact request flag")
        }
        print("AtlasProjectLinksBridgeTests: both projections, full-width IDs, callbacks and buffer ownership passed")
    }
    private static func tryValue(_ body: () throws -> String?) -> String? { try! body() }
}
