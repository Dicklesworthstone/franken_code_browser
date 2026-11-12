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
        print("AtlasSavedIndexBridgeTests: public-header signatures, matching/count flags, full-width IDs, exact path bytes, pin admission and buffer ownership passed")
    }
}
