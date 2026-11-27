import Foundation

extension AtlasReaderTransport {
    static let native = SavedRepositoryFixture().readerTransport()
}
@_silgen_name("fixture_saved_index_mode") private func mode(_ value: Int32)
@_silgen_name("fixture_saved_index_count") private func count(_ index: UInt64) -> UInt64

@main private enum AtlasSavedIndexBridgeTests {
    static func main() {
        let transport = AtlasSavedTransport.native, owner = UInt64.max - 10, generation = UInt64.max - 9
        let path = "/index-e\u{301}.fcbd", pin = String(repeating: "a", count: 64)
        precondition(transport.create() == owner)
        precondition(transport.run(owner, .attachIndex(generation: generation, path: path, pin: pin)) == "{}")
        precondition(transport.run(owner, .detachIndex(generation: generation)) == "{}")
        precondition(transport.run(owner, .info) == "{}")
        precondition(transport.cancel(owner) && transport.close(owner))
        precondition((0...5).allSatisfy { count(UInt64($0)) == 1 } && count(6) == 3)
        for invalid in ["relative.fcbd", "/a\0.fcbd", "/" + String(repeating: "x", count: 16_384)] {
            precondition(transport.run(owner, .attachIndex(generation: generation, path: invalid, pin: pin)) == nil)
        }
        for invalid in ["", String(repeating: "a", count: 63), String(repeating: "A", count: 64),
                        String(repeating: "g", count: 64), "\0" + String(repeating: "a", count: 63)] {
            precondition(transport.run(owner, .attachIndex(generation: generation, path: path, pin: invalid)) == nil)
        }
        precondition(transport.run(owner, .attachIndex(generation: 0, path: path, pin: pin)) == nil)
        precondition(transport.run(owner, .detachIndex(generation: 0)) == nil)
        precondition(count(1) == 1 && count(2) == 1 && count(6) == 3, "invalid input crossed the foreign boundary")
        mode(1)
        precondition(transport.run(owner, .attachIndex(generation: generation, path: path, pin: pin)) == nil && count(6) == 3)
        for invalid in [Int32(2), 3, 4] {
            mode(invalid)
            precondition(transport.run(owner, .attachIndex(generation: generation, path: path, pin: pin)) == nil)
            precondition(transport.run(owner, .detachIndex(generation: generation)) == nil)
        }
        precondition(count(6) == 9, "every rejected non-null response must be freed exactly once")
        mode(0)
        for retained in [UInt64(0), 4096] {
            precondition(transport.run(owner, .countedSearch(generation: generation,
                needle: "e\u{301}", matches: retained)) == "{}")
            precondition(transport.run(owner, .countedExpressionSearch(generation: generation,
                query: "e\u{301} path:src/", matches: retained, scanBytes: 67_108_864)) == "{}")
        }
        precondition(count(7) == 2 && count(8) == 2 && count(6) == 13)
        precondition(transport.run(owner, .countedSearch(generation: 0, needle: "e\u{301}", matches: 0)) == nil)
        precondition(transport.run(owner, .countedSearch(generation: generation, needle: "", matches: 0)) == nil)
        precondition(transport.run(owner, .countedSearch(generation: generation, needle: "e\u{301}", matches: 4097)) == nil)
        precondition(transport.run(owner, .countedExpressionSearch(generation: generation,
            query: "e\u{301} path:src/", matches: 0, scanBytes: 67_108_865)) == nil)
        precondition(count(7) == 2 && count(8) == 2 && count(6) == 13,
            "invalid counted queries crossed the foreign boundary")
        for invalid in [Int32(1), 2, 3, 4] {
            mode(invalid)
            precondition(transport.run(owner, .countedSearch(generation: generation, needle: "e\u{301}", matches: 0)) == nil)
            precondition(transport.run(owner, .countedExpressionSearch(generation: generation,
                query: "e\u{301} path:src/", matches: 0, scanBytes: 67_108_864)) == nil)
        }
        precondition(count(6) == 19, "counted response ownership leaked or double-freed")
        mode(0)
        for (matchCase, countAll, retained) in [(false, false, UInt64(4096)), (false, true, 0), (true, true, 4096)] {
            precondition(transport.run(owner, .matchingSearch(generation: generation, needle: "e\u{301}",
                caseSensitive: matchCase, countAllMatches: countAll, matches: retained)) == "{}")
        }
        precondition(count(9) == 3 && count(6) == 22)
        for (queryGeneration, needle, countAll, retained) in [
            (UInt64(0), "e\u{301}", true, UInt64(0)), (generation, "", true, 0),
            (generation, "e\0", true, 0), (generation, String(repeating: "x", count: 1025), true, 0),
            (generation, "e\u{301}", true, 4097), (generation, "e\u{301}", false, 0)
        ] {
            precondition(transport.run(owner, .matchingSearch(generation: queryGeneration, needle: needle,
                caseSensitive: false, countAllMatches: countAll, matches: retained)) == nil)
        }
        precondition(count(9) == 3 && count(6) == 22, "invalid matching queries crossed the foreign boundary")
        for invalid in [Int32(1), 2, 3, 4] {
            mode(invalid)
            precondition(transport.run(owner, .matchingSearch(generation: generation, needle: "e\u{301}",
                caseSensitive: false, countAllMatches: true, matches: 0)) == nil)
        }
        precondition(count(9) == 7 && count(6) == 25, "matching response ownership leaked or double-freed")
        mode(0)
        precondition(transport.run(owner, .prepareImports(generation: generation, imports: 4096, bytes: 33_554_432)) == "{}")
        precondition(transport.run(owner, .imports(generation: generation, direction: .incoming, member: 65_535, start: 4096, limit: 128)) == "{}")
        precondition(transport.run(owner, .clearImports(generation: generation)) == "{}")
        precondition(transport.run(owner, .openImport(reader: UInt64.max - 8, generation: generation, importID: 4096, side: .target)) == "{}")
        precondition((10...13).allSatisfy { count(UInt64($0)) == 1 } && count(6) == 29)
        let invalidImports: [AtlasSavedRequest] = [
            .prepareImports(generation: 0, imports: 4096, bytes: 33_554_432),
            .prepareImports(generation: generation, imports: 0, bytes: 33_554_432),
            .prepareImports(generation: generation, imports: 4096, bytes: 33_554_433),
            .imports(generation: generation, direction: .all, member: 0, start: 0, limit: 64),
            .imports(generation: generation, direction: .incoming, member: nil, start: 0, limit: 64),
            .imports(generation: generation, direction: .incoming, member: 65_536, start: 0, limit: 64),
            .imports(generation: generation, direction: .incoming, member: 1, start: 0, limit: 129),
            .openImport(reader: owner, generation: generation, importID: 1, side: .target),
            .openImport(reader: UInt64.max - 8, generation: generation, importID: 4097, side: .target)
        ]
        for request in invalidImports { precondition(transport.run(owner, request) == nil) }
        precondition((10...13).allSatisfy { count(UInt64($0)) == 1 } && count(6) == 29,
            "invalid import request crossed the foreign boundary")
        let query: [UInt8] = [255, 47, 97]
        precondition(transport.run(owner, .pathSearch(generation: generation, query: query,
            mode: .prefix, caseSensitive: false, results: 256, work: 4_294_967_296)) == "{}")
        precondition(transport.run(owner, .pathResults(generation: generation, start: 256, limit: 128)) == "{}")
        precondition(transport.run(owner, .clearPaths(generation: generation)) == "{}")
        precondition(transport.run(owner, .openPathHit(reader: UInt64.max - 8, generation: generation, hit: 256)) == "{}")
        precondition((14...17).allSatisfy { count(UInt64($0)) == 1 } && count(6) == 33)
        let invalidPaths: [AtlasSavedRequest] = [
            .pathSearch(generation: 0, query: query, mode: .fuzzy, caseSensitive: false, results: 256, work: 1),
            .pathSearch(generation: generation, query: [], mode: .fuzzy, caseSensitive: false, results: 256, work: 1),
            .pathSearch(generation: generation, query: [0], mode: .fuzzy, caseSensitive: false, results: 256, work: 1),
            .pathSearch(generation: generation, query: Array(repeating: 1, count: 257), mode: .fuzzy, caseSensitive: false, results: 256, work: 1),
            .pathSearch(generation: generation, query: query, mode: .fuzzy, caseSensitive: false, results: 257, work: 1),
            .pathSearch(generation: generation, query: query, mode: .fuzzy, caseSensitive: false, results: 256, work: 4_294_967_297),
            .pathResults(generation: generation, start: 257, limit: 64), .pathResults(generation: generation, start: 0, limit: 0),
            .clearPaths(generation: 0), .openPathHit(reader: owner, generation: generation, hit: 1)
        ]
        for request in invalidPaths { precondition(transport.run(owner, request) == nil) }
        precondition((14...17).allSatisfy { count(UInt64($0)) == 1 } && count(6) == 33,
            "invalid filename request crossed the foreign boundary")
        for limits in [AtlasSavedIndexBuildLimits.standard, .init(fileBytes: 0, sourceBytes: 0, grams: 0)] {
            precondition(transport.run(owner, .buildIndex(operation: generation, destination: path, limits: limits)) == "{}")
        }
        precondition(transport.run(owner, .indexBuildReceipt(operation: generation)) == "{}")
        precondition(count(18) == 2 && count(19) == 1 && count(6) == 36)
        let invalidBuilds: [AtlasSavedRequest] = [
            .buildIndex(operation: 0, destination: path, limits: .standard),
            .buildIndex(operation: generation, destination: "/", limits: .standard),
            .buildIndex(operation: generation, destination: "/a/../index.fcbd", limits: .standard),
            .buildIndex(operation: generation, destination: "/a\0.fcbd", limits: .standard),
            .buildIndex(operation: generation, destination: "/" + String(repeating: "x", count: 16_384), limits: .standard),
            .buildIndex(operation: generation, destination: path, limits: .init(fileBytes: 262_145, sourceBytes: 1, grams: 1)),
            .buildIndex(operation: generation, destination: path, limits: .init(fileBytes: 1, sourceBytes: 67_108_865, grams: 1)),
            .buildIndex(operation: generation, destination: path, limits: .init(fileBytes: 1, sourceBytes: 1, grams: 2_097_153)),
            .indexBuildReceipt(operation: 0)
        ]
        for request in invalidBuilds { precondition(transport.run(owner, request) == nil) }
        precondition(count(18) == 2 && count(19) == 1 && count(6) == 36,
            "invalid publication crossed the foreign boundary")
        for invalid in [Int32(1), 2, 3, 4] {
            mode(invalid)
            precondition(transport.run(owner, .buildIndex(operation: generation, destination: path, limits: .standard)) == nil)
            precondition(transport.run(owner, .indexBuildReceipt(operation: generation)) == nil)
        }
        precondition(count(18) == 6 && count(19) == 5 && count(6) == 42,
            "publication response ownership leaked or double-freed")
        mode(0)
        for (caseSensitive, countAll, retained) in [(false, false, UInt64(4096)), (false, true, 0), (true, true, 4096)] {
            precondition(transport.run(owner, .matchingExpressionSearch(generation: generation,
                query: "e\u{301} path:src/", caseSensitive: caseSensitive, countAllMatches: countAll,
                matches: retained, scanBytes: 67_108_864)) == "{}")
        }
        precondition(count(20) == 3 && count(6) == 45)
        for (queryGeneration, query, countAll, retained, bytes) in [
            (UInt64(0), "e\u{301} path:src/", true, UInt64(0), UInt64(67_108_864)),
            (generation, "", true, 0, 67_108_864), (generation, "e\0", true, 0, 67_108_864),
            (generation, String(repeating: "x", count: 1025), true, 0, 67_108_864),
            (generation, "e\u{301} path:src/", true, 4097, 67_108_864),
            (generation, "e\u{301} path:src/", false, 0, 67_108_864),
            (generation, "e\u{301} path:src/", true, 0, 67_108_865)
        ] {
            precondition(transport.run(owner, .matchingExpressionSearch(generation: queryGeneration,
                query: query, caseSensitive: false, countAllMatches: countAll,
                matches: retained, scanBytes: bytes)) == nil)
        }
        precondition(count(20) == 3 && count(6) == 45, "invalid matching expressions crossed the foreign boundary")
        for invalid in [Int32(1), 2, 3, 4] {
            mode(invalid)
            precondition(transport.run(owner, .matchingExpressionSearch(generation: generation,
                query: "e\u{301} path:src/", caseSensitive: false, countAllMatches: true,
                matches: 0, scanBytes: 67_108_864)) == nil)
        }
        precondition(count(20) == 7 && count(6) == 48, "matching expression response ownership leaked or double-freed")
        mode(0)
        let windows: [AtlasSavedRequest] = [
            .searchWindow(generation: generation, needle: "e\u{301}", caseSensitive: false,
                countAllMatches: true, matches: 4096, skipMatches: 9_007_199_254_740_993),
            .expressionWindow(generation: generation, query: "e\u{301} path:src/", caseSensitive: true,
                countAllMatches: false, matches: 4096, scanBytes: 67_108_864, skipMatches: UInt64.max),
            .structureWindow(generation: generation, mode: .occurrences, nameMode: .exact, query: "e\u{301}",
                matches: 4096, sourceBytes: 67_108_864, skipMatches: 9_007_199_254_740_993,
                policy: .init(caseSensitive: true, countAllMatches: true))
        ]
        for request in windows { precondition(transport.run(owner, request) == "{}") }
        precondition((21...23).allSatisfy { count(UInt64($0)) == 1 } && count(6) == 51)
        let invalidWindows: [AtlasSavedRequest] = [
            .searchWindow(generation: 0, needle: "e\u{301}", caseSensitive: true, countAllMatches: true, matches: 4096, skipMatches: 0),
            .searchWindow(generation: generation, needle: "", caseSensitive: true, countAllMatches: true, matches: 4096, skipMatches: 0),
            .searchWindow(generation: generation, needle: "e\u{301}", caseSensitive: true, countAllMatches: false, matches: 0, skipMatches: 0),
            .searchWindow(generation: generation, needle: "e\u{301}", caseSensitive: true, countAllMatches: true, matches: 4097, skipMatches: 0),
            .expressionWindow(generation: generation, query: "a\0", caseSensitive: false,
                countAllMatches: true, matches: 4096, scanBytes: 1, skipMatches: 0),
            .expressionWindow(generation: generation, query: "a", caseSensitive: false,
                countAllMatches: true, matches: 4096, scanBytes: 67_108_865, skipMatches: 0),
            .structureWindow(generation: generation, mode: .literal, nameMode: .exact, query: "a",
                matches: 4096, sourceBytes: 1, skipMatches: 0),
            .structureWindow(generation: generation, mode: .occurrences, nameMode: .contains, query: "a",
                matches: 4096, sourceBytes: 1, skipMatches: 0),
            .structureWindow(generation: generation, mode: .occurrences, nameMode: .prefix, query: "a",
                matches: 1, sourceBytes: 1, skipMatches: 0, policy: .init(caseSensitive: false, countAllMatches: true)),
            .structureWindow(generation: generation, mode: .symbols, nameMode: .exact, query: "a",
                matches: 0, sourceBytes: 1, skipMatches: 0),
            .structureWindow(generation: generation, mode: .headings, nameMode: .exact, query: "a",
                matches: 4096, sourceBytes: 67_108_865, skipMatches: 0)
        ]
        for request in invalidWindows { precondition(transport.run(owner, request) == nil) }
        precondition((21...23).allSatisfy { count(UInt64($0)) == 1 } && count(6) == 51,
            "invalid saved result window crossed the C boundary")
        for invalid in [Int32(1), 2, 3, 4] {
            mode(invalid)
            for request in windows { precondition(transport.run(owner, request) == nil) }
        }
        precondition((21...23).allSatisfy { count(UInt64($0)) == 5 } && count(6) == 60,
            "saved result-window ownership leaked or double-freed")
        mode(0)
        precondition(transport.run(owner, .structureSearch(generation: generation, mode: .headings, nameMode: .contains,
            query: "CAFÉ", matches: 4096, sourceBytes: 67_108_864,
            policy: .init(caseSensitive: false, countAllMatches: true))) == "{}")
        precondition(count(24) == 1 && count(6) == 61)
        let lowercaseOccurrences: [AtlasSavedRequest] = [
            .structureSearch(generation: generation, mode: .occurrences, nameMode: .exact, query: "E\u{301}",
                matches: 4096, sourceBytes: 67_108_864, policy: .init(caseSensitive: false, countAllMatches: true)),
            .structureWindow(generation: generation, mode: .occurrences, nameMode: .exact, query: "E\u{301}",
                matches: 4096, sourceBytes: 67_108_864, skipMatches: 9_007_199_254_740_993,
                policy: .init(caseSensitive: false, countAllMatches: true))
        ]
        for request in lowercaseOccurrences { precondition(transport.run(owner, request) == "{}") }
        precondition(count(24) == 2 && count(23) == 6 && count(6) == 63,
            "lowercase whole-token queries and later windows did not reach the native ABI")
        precondition(transport.run(owner, .structureSearch(generation: generation, mode: .occurrences,
            nameMode: .contains, query: "E\u{301}", matches: 4096, sourceBytes: 67_108_864,
            policy: .init(caseSensitive: false, countAllMatches: true))) == nil)
        precondition(count(24) == 2 && count(23) == 6 && count(6) == 63,
            "lowercase occurrence matching must still reject substring name modes")
        for invalid in [Int32(1), 2, 3, 4] {
            mode(invalid)
            for request in lowercaseOccurrences { precondition(transport.run(owner, request) == nil) }
        }
        precondition(count(24) == 6 && count(23) == 10 && count(6) == 69,
            "lowercase occurrence response ownership leaked or double-freed")
        mode(0)
        precondition(transport.run(owner, .structureSearch(generation: generation, mode: .occurrences,
            nameMode: .exact, query: "target", matches: 100, sourceBytes: 200,
            policy: .init(caseSensitive: false, countAllMatches: true, scope: .member([115, 255])))) == "{}")
        precondition(transport.run(owner, .structureWindow(generation: generation, mode: .occurrences,
            nameMode: .exact, query: "target", matches: 100, sourceBytes: 200, skipMatches: UInt64.max,
            policy: .init(caseSensitive: false, countAllMatches: true, scope: .directory([115, 255])))) == "{}")
        precondition(count(25) == 2 && count(6) == 71)
        let scopedWindows: [AtlasSavedRequest] = [
            .scopedSearchWindow(generation: generation, needle: "e\u{301}", caseSensitive: false,
                countAllMatches: true, matches: 4096, skipMatches: 9_007_199_254_740_993,
                scope: .directory([115, 255])),
            .scopedExpressionWindow(generation: generation, query: "e\u{301} path:src/", caseSensitive: true,
                countAllMatches: false, matches: 4096, scanBytes: 67_108_864, skipMatches: UInt64.max,
                scope: .member([115, 255, 47, 97]))
        ]
        for request in scopedWindows { precondition(transport.run(owner, request) == "{}") }
        precondition(count(26) == 1 && count(27) == 1 && count(6) == 73)
        let invalidScopes: [AtlasSavedRequest] = [
            .scopedSearchWindow(generation: generation, needle: "ana", caseSensitive: true,
                countAllMatches: true, matches: 4096, skipMatches: 0, scope: .directory([])),
            .scopedSearchWindow(generation: 0, needle: "ana", caseSensitive: true,
                countAllMatches: true, matches: 4096, skipMatches: 0, scope: .member([97])),
            .scopedSearchWindow(generation: generation, needle: "ana", caseSensitive: true,
                countAllMatches: false, matches: 0, skipMatches: 0, scope: .member([97])),
            .scopedSearchWindow(generation: generation, needle: "ana", caseSensitive: true,
                countAllMatches: true, matches: 4096, skipMatches: 0,
                scope: .member(Array(repeating: 97, count: 2049))),
            .scopedExpressionWindow(generation: generation, query: "ana", caseSensitive: false,
                countAllMatches: true, matches: 4096, scanBytes: 67_108_865, skipMatches: 0,
                scope: .member([97])),
            .scopedExpressionWindow(generation: generation, query: "ana\0", caseSensitive: false,
                countAllMatches: true, matches: 4096, scanBytes: 64, skipMatches: 0, scope: .member([97])),
            .scopedExpressionWindow(generation: generation, query: "ana", caseSensitive: false,
                countAllMatches: true, matches: 4096, scanBytes: 64, skipMatches: 0, scope: .member([]))
        ]
        for request in invalidScopes { precondition(transport.run(owner, request) == nil) }
        precondition(count(26) == 1 && count(27) == 1 && count(6) == 73,
            "invalid scoped search crossed the foreign boundary")
        for invalid in [Int32(1), 2, 3, 4] {
            mode(invalid)
            for request in scopedWindows { precondition(transport.run(owner, request) == nil) }
        }
        precondition(count(26) == 5 && count(27) == 5 && count(6) == 79,
            "scoped result-window response ownership leaked or double-freed")
        mode(0)
        precondition(transport.run(owner, .scopedSearchWindow(generation: generation, needle: "e\u{301}",
            caseSensitive: false, countAllMatches: true, matches: 4096, skipMatches: 9_007_199_254_740_993,
            scope: .archive)) == "{}")
        precondition(transport.run(owner, .scopedExpressionWindow(generation: generation, query: "e\u{301} path:src/",
            caseSensitive: true, countAllMatches: false, matches: 4096, scanBytes: 67_108_864,
            skipMatches: UInt64.max, scope: .archive)) == "{}")
        precondition(count(26) == 6 && count(27) == 6 && count(6) == 81,
            "whole-archive scope must cross the ABI as null, zero-length path")

        print("AtlasSavedIndexBridgeTests: public-header signatures, publication/import/path/matching/window requests, native bytes, full-width IDs and offsets, and buffer ownership passed")
    }
}
