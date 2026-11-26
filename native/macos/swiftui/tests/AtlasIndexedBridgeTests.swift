import Foundation
@_silgen_name("indexed_fixture_reset") private func reset()
@_silgen_name("indexed_fixture_mode") private func mode(_ value: Int32)
@_silgen_name("indexed_fixture_calls") private func calls() -> UInt32
@_silgen_name("indexed_fixture_live") private func live() -> UInt32
@_silgen_name("indexed_fixture_policy") private func selectedPolicy() -> UInt64
@_silgen_name("indexed_fixture_directory_length") private func directoryLength() -> UInt64
@_silgen_name("indexed_fixture_file_length") private func fileLength() -> UInt64
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
        let beforePolicy = calls()
        for policy in [AtlasDiscoveryPolicy.repositoryRules, .includeExcluded] {
            check(try t.openWithPolicy!(h, "/repo", policy, flag) == "{\"owned\":true}")
            check(selectedPolicy() == policy.rawValue && live() == 0)
        }
        check(calls() == beforePolicy + 2)
        let directory = try AtlasSearchCatalogScope(directory: [122, 255, 92, 45])
        let beforeDirectory = calls()
        for policy in [AtlasDiscoveryPolicy.repositoryRules, .includeExcluded] {
            check(try t.openDirectoryWithPolicy!(h, "/repo", directory, policy, flag) == "{\"owned\":true}")
            check(selectedPolicy() == policy.rawValue && directoryLength() == 4 && live() == 0)
        }
        check(calls() == beforeDirectory + 2)
        do {
            _ = try t.openDirectoryWithPolicy!(h, "/repo", .workspace, .repositoryRules, flag)
            preconditionFailure("A workspace request entered the directory ABI")
        } catch AtlasSearchError.invalidCatalogScope { checks += 1 }
        check(calls() == beforeDirectory + 2)
        for bad: Int32 in [1, 2, 3, 4] {
            mode(bad)
            let directoryFlag = AtlasSearchCancellation()
            do {
                let result = try t.openDirectoryWithPolicy!(h, "/repo", directory, .repositoryRules, directoryFlag)
                check(bad == 1 && result == nil)
            } catch AtlasSearchError.invalidResponse { check(bad == 2 || bad == 3) }
            catch AtlasSearchError.canceled { check(bad == 4 && directoryFlag.isCanceled) }
            check(live() == 0)
        }
        let canceledDirectory = AtlasSearchCancellation()
        canceledDirectory.cancel()
        let beforeCanceledDirectory = calls()
        do {
            _ = try t.openDirectoryWithPolicy!(h, "/repo", directory, .repositoryRules, canceledDirectory)
            preconditionFailure("A canceled directory request reached C")
        } catch AtlasSearchError.canceled { checks += 1 }
        check(calls() == beforeCanceledDirectory && live() == 0)
        mode(0)
        let exactFile = try AtlasSearchCatalogScope(file: [122, 255, 92, 45, 47, 97, 46, 114, 115])
        let beforeFile = calls()
        for policy in [AtlasDiscoveryPolicy.repositoryRules, .includeExcluded] {
            check(try t.openFileWithPolicy!(h, "/repo", exactFile, policy, flag) == "{\"owned\":true}")
            check(selectedPolicy() == policy.rawValue && fileLength() == 9 && live() == 0)
        }
        check(calls() == beforeFile + 2)
        for invalid in [AtlasSearchCatalogScope.workspace, directory] {
            do {
                _ = try t.openFileWithPolicy!(h, "/repo", invalid, .repositoryRules, flag)
                preconditionFailure("A non-file scope entered the exact-file ABI")
            } catch AtlasSearchError.invalidCatalogScope { checks += 1 }
        }
        do {
            _ = try t.openDirectoryWithPolicy!(h, "/repo", exactFile, .repositoryRules, flag)
            preconditionFailure("A file scope entered the directory ABI")
        } catch AtlasSearchError.invalidCatalogScope { checks += 1 }
        check(calls() == beforeFile + 2)
        for bad: Int32 in [1, 2, 3, 4] {
            mode(bad)
            let fileFlag = AtlasSearchCancellation()
            do {
                let result = try t.openFileWithPolicy!(h, "/repo", exactFile, .includeExcluded, fileFlag)
                check(bad == 1 && result == nil)
            } catch AtlasSearchError.invalidResponse { check(bad == 2 || bad == 3) }
            catch AtlasSearchError.canceled { check(bad == 4 && fileFlag.isCanceled) }
            check(live() == 0)
        }
        let canceledFile = AtlasSearchCancellation(); canceledFile.cancel()
        let beforeCanceledFile = calls()
        do {
            _ = try t.openFileWithPolicy!(h, "/repo", exactFile, .repositoryRules, canceledFile)
            preconditionFailure("A canceled exact-file request reached C")
        } catch AtlasSearchError.canceled { checks += 1 }
        check(calls() == beforeCanceledFile && live() == 0)
        mode(0)
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
        let beforeStructure = calls()
        check(try t.structureQueryBegin!(h, 70, 1, .symbols, .prefix, "SomeName", .all, .workspace) != nil)
        check(try t.structureQueryBegin!(h, 71, 1, .headings, .contains, "SomeName",
            AtlasSearchFileScope(extensions: ["md"]), AtlasSearchPathScope(kind: .directory, path: "docs")) != nil)
        check(try t.structureQueryBegin!(h, 72, 1, .occurrences, .contains, "SomeName", .all,
            AtlasSearchPathScope(kind: .file, path: "src/exact.rs")) != nil)
        check(live() == 0 && calls() == beforeStructure + 3)
        do { _ = try t.structureQueryBegin!(h, 70, 1, .text, .contains, "SomeName", .all, .workspace)
            preconditionFailure("Text mode admitted to structural bridge") }
        catch AtlasSearchError.invalidRequest { checks += 1 }
        check(calls() == beforeStructure + 3)
        let beforeMatchingStructure = calls()
        check(try t.matchingStructureQueryBegin!(h, 73, 1, .occurrences, .prefix, "SomeName", .all,
            AtlasSearchPathScope(kind: .file, path: "src/exact.rs"), false) != nil)
        check(try t.matchingStructureQueryBegin!(h, 70, 1, .symbols, .prefix, "SomeName", .all, .workspace, true) != nil)
        check(try t.matchingStructureWindowQueryBegin!(h, 83, 1, .occurrences, .contains, "SomeName", .all,
            .workspace, false, true, 1000) != nil)
        check(try t.structureWindowQueryBegin!(h, 84, 1, .occurrences, .prefix, "SomeName", .all,
            .workspace, false, 0) != nil)
        check(live() == 0 && calls() == beforeMatchingStructure + 4)
        do { _ = try t.matchingStructureQueryBegin!(h, 73, 1, .symbols, .prefix, "SomeName", .all, .workspace, false)
            preconditionFailure("Unsupported lowercase declaration mode crossed the captured structural ABI") }
        catch AtlasSearchError.invalidRequest { checks += 1 }
        do { _ = try t.matchingStructureWindowQueryBegin!(h, 83, 1, .occurrences, .exact, "SomeName", .all,
                .workspace, false, true, UInt64.max)
            preconditionFailure("Unbounded occurrence window crossed the ABI") }
        catch AtlasSearchError.invalidRequest { checks += 1 }
        check(calls() == beforeMatchingStructure + 4)
        let beforeWindows = calls()
        check(try t.windowQueryBegin!(h, 80, 1, "exact text", .all, .workspace, true, false, true, 1000) != nil)
        check(try t.windowQueryBegin!(h, 81, 1, expression, AtlasSearchFileScope(extensions: ["rs"]),
            AtlasSearchPathScope(kind: .directory, path: "src"), false, true, false, 2000) != nil)
        check(live() == 0 && calls() == beforeWindows + 2)
        #if canImport(CryptoKit)
        check(try t.windowQueryBegin!(h, 82, 1, "exact text", .all,
            AtlasSearchPathScope(visiblePaths: [[255, 46, 114, 115], Array("a.rs".utf8)]),
            false, false, false, 0) != nil)
        check(live() == 0 && calls() == beforeWindows + 3)
        #endif
        let beforeInvalidWindow = calls()
        do { _ = try t.windowQueryBegin!(h, 80, 1, "exact text", .all, .workspace,
                true, false, true, UInt64.max)
            preconditionFailure("Unbounded window offset crossed the ABI") }
        catch AtlasSearchError.invalidRequest { checks += 1 }
        check(calls() == beforeInvalidWindow)
        mode(1)
        check(try t.windowQueryBegin!(h, 80, 1, "exact text", .all, .workspace,
            true, false, true, 1000) == nil && live() == 0)
        for bad: Int32 in [2, 3] {
            mode(bad)
            do { _ = try t.windowQueryBegin!(h, 80, 1, "exact text", .all, .workspace,
                    true, false, true, 1000)
                preconditionFailure("Malformed window response accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            check(live() == 0)
        }
        mode(1); check(try t.structureQueryBegin!(h, 70, 1, .symbols, .prefix, "SomeName", .all, .workspace) == nil && live() == 0)
        for bad: Int32 in [2, 3] {
            mode(bad)
            do { _ = try t.structureQueryBegin!(h, 70, 1, .symbols, .prefix, "SomeName", .all, .workspace)
                preconditionFailure("Malformed structural response accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            check(live() == 0)
        }
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
