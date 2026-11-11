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
        check(try t.scopedQueryBegin!(h, 45, 1, "exact text", AtlasFileScope.rust.searchScope()) != nil)
        check(try t.scopedQueryBegin!(h, 46, 1, "exact text", AtlasSearchFileScope(extensions: [])) != nil)
        check(live() == 0 && calls() == 10)
        check(try t.pathScopedQueryBegin!(h, 47, 1, "exact text", .all,
            AtlasSearchPathScope(kind: .directory, path: "src")) != nil)
        check(try t.pathScopedQueryBegin!(h, 48, 1, "exact text", AtlasSearchFileScope(extensions: ["rs"]),
            AtlasSearchPathScope(kind: .file, path: "src/exact.rs")) != nil)
        check(live() == 0 && calls() == 12)
        check(try t.matchingQueryBegin!(h, 49, 1, "exact text", .all, .workspace, false) != nil)
        check(try t.matchingQueryBegin!(h, 50, 1, "exact text", AtlasSearchFileScope(extensions: ["rs"]),
            AtlasSearchPathScope(kind: .file, path: "src/exact.rs"), false) != nil)
        check(try t.matchingQueryBegin!(h, 51, 1, "exact text", .all,
            AtlasSearchPathScope(kind: .directory, path: "src"), true) != nil)
        check(live() == 0 && calls() == 15)
        check(try t.scopedIndexBegin!(h, 1, .all, .workspace) != nil)
        check(try t.scopedIndexBegin!(h, 3, AtlasSearchFileScope(extensions: ["rs"]),
            AtlasSearchPathScope(kind: .file, path: "zz-last.rs")) != nil)
        check(try t.scopedIndexBegin!(h, 4, AtlasSearchFileScope(extensions: []),
            AtlasSearchPathScope(kind: .directory, path: "src")) != nil)
        check(live() == 0 && calls() == 18)
        #if canImport(CryptoKit)
        check(try t.scopedIndexBegin!(h, 5, .all,
            AtlasSearchPathScope(visiblePaths: [[255, 46, 114, 115], Array("a.rs".utf8)])) != nil)
        check(try t.scopedIndexBegin!(h, 6, .all, AtlasSearchPathScope(visiblePaths: [])) != nil)
        check(live() == 0 && calls() == 20)
        #endif
        let beforeExpressions = calls()
        let expression = "\"exact text\" required -excluded lang:rs"
        check(try t.expressionQueryBegin!(h, 52, 1, expression, .all, .workspace, true) != nil)
        check(try t.expressionQueryBegin!(h, 53, 1, expression, AtlasSearchFileScope(extensions: ["rs"]),
            AtlasSearchPathScope(kind: .directory, path: "src"), false) != nil)
        check(live() == 0 && calls() == beforeExpressions + 2)
        let beforeExpandedCounts = calls()
        check(try t.countedQueryBegin!(h, 60, 1, "exact text", .all, .workspace, false) != nil)
        check(try t.countedExpressionQueryBegin!(h, 62, 1, expression, .all, .workspace, true) != nil)
        check(try t.countedExpressionQueryBegin!(h, 63, 1, expression, AtlasSearchFileScope(extensions: ["rs"]),
            AtlasSearchPathScope(kind: .directory, path: "src"), false) != nil)
        check(live() == 0 && calls() == beforeExpandedCounts + 3)
        let beforeCounts = calls()
        check(try t.countedQueryBegin!(h, 54, 1, "exact text", .all, .workspace, true) != nil)
        check(try t.countedQueryBegin!(h, 55, 1, "exact text", AtlasSearchFileScope(extensions: ["rs"]),
            AtlasSearchPathScope(kind: .file, path: "src/exact.rs"), true) != nil)
        check(try t.countedQueryBegin!(h, 56, 1, "exact text", .all,
            AtlasSearchPathScope(kind: .directory, path: "src"), true) != nil)
        check(try t.countedQueryBegin!(h, 59, 1, "exact text", AtlasSearchFileScope(extensions: []), .workspace, true) != nil)
        check(live() == 0 && calls() == beforeCounts + 4)
        #if canImport(CryptoKit)
        check(try t.countedQueryBegin!(h, 57, 1, "exact text", .all,
            AtlasSearchPathScope(visiblePaths: [[255, 46, 114, 115], Array("a.rs".utf8)]), true) != nil)
        check(try t.countedQueryBegin!(h, 58, 1, "exact text", .all, AtlasSearchPathScope(visiblePaths: []), true) != nil)
        check(live() == 0 && calls() == beforeCounts + 6)
        #endif
        mode(1); check(try t.countedQueryBegin!(h, 54, 1, "exact text", .all, .workspace, true) == nil && live() == 0)
        for bad: Int32 in [2, 3] {
            mode(bad)
            do {
                _ = try t.countedQueryBegin!(h, 54, 1, "exact text", .all, .workspace, true)
                preconditionFailure("Malformed counted response accepted")
            } catch AtlasSearchError.invalidResponse { checks += 1 }
            check(live() == 0)
        }
        mode(1); check(try t.expressionQueryBegin!(h, 52, 1, expression, .all, .workspace, true) == nil && live() == 0)
        for bad: Int32 in [2, 3] {
            mode(bad)
            do {
                _ = try t.expressionQueryBegin!(h, 52, 1, expression, .all, .workspace, true)
                preconditionFailure("Malformed expression response accepted")
            } catch AtlasSearchError.invalidResponse { checks += 1 }
            check(live() == 0)
        }
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
