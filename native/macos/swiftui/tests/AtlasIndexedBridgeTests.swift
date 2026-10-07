import Foundation
@_silgen_name("indexed_fixture_reset") private func reset()
@_silgen_name("indexed_fixture_mode") private func mode(_ value: Int32)
@_silgen_name("indexed_fixture_calls") private func calls() -> UInt32
@_silgen_name("indexed_fixture_live") private func live() -> UInt32
@_cdecl("indexed_fixture_cancel") func cancelIndexedFixture(_ context: UnsafeMutableRawPointer?) {
    guard let context else { preconditionFailure("Missing cancellation context") }
    Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().cancel()
}
@main private enum AtlasIndexedBridgeTests {
    @MainActor static func main() throws {
        reset()
        var checks = 0
        func check(_ condition: Bool) { precondition(condition); checks += 1 }
        let t = AtlasIndexedSearchTransport.native, h = t.create(), flag = AtlasSearchCancellation()
        check(h == 18_446_744_073_709_551_500)
        check(try t.open(h, "/repo", flag) == "{\"owned\":true}")
        check(try t.indexBegin(h, 1) != nil)
        check(try t.indexStep(h, 1, flag) != nil)
        check(try t.queryBegin(h, 44, 1, "exact text") != nil)
        check(try t.queryStep(h, 44, flag) != nil)
        check(try t.page(h, 44, 64, 128) != nil)
        check(try t.sourceReader(h, 9_007_199_254_740_999, 1, 321, 1) != nil)
        check(live() == 0 && calls() == 8)
        mode(1); check(try t.indexBegin(h, 1) == nil && live() == 0)
        for bad: Int32 in [2, 3] {
            mode(bad)
            do { _ = try t.indexBegin(h, 1); preconditionFailure("Malformed response accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            check(live() == 0)
        }
        mode(4)
        do { _ = try t.indexStep(h, 1, flag); preconditionFailure("Racing cancellation ignored") }
        catch AtlasSearchError.canceled { checks += 1 }
        check(flag.isCanceled && live() == 0)
        let before = calls()
        do { _ = try t.queryStep(h, 44, flag); preconditionFailure("Pre-canceled call admitted") }
        catch AtlasSearchError.canceled { checks += 1 }
        check(calls() == before)
        mode(0); check(t.close(h))
        check(live() == 0)
        print("AtlasIndexedBridgeTests: \(checks) checks passed")
    }
}
