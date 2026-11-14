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
        print("AtlasSavedIndexBridgeTests: public-header signatures, publication/import/path/matching requests, native bytes, full-width IDs and buffer ownership passed")
    }
}
