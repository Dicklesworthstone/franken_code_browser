// swiftc -swift-version 6 AtlasSearch.swift tests/AtlasSearchNavigationTests.swift \
//   -o /tmp/fcb-search-navigation-tests
import Foundation

@main struct AtlasSearchNavigationTests {
    static func main() {
        let generation = UUID(), revision = UUID()
        func context(_ query: String = "needle") -> AtlasSearchContext {
            AtlasSearchContext(root: "/project", query: query, loadGeneration: generation,
                atlasRevision: revision, scope: "All files", customExtensions: "")
        }
        func hit(_ id: Int, path: String? = "file.rs", digest: String? = String(repeating: "a", count: 64)) -> SearchHit {
            SearchHit(id: id, path: path ?? "escaped name", sourcePath: path, start: 0, end: 1,
                captureSHA256: digest, captureByteLength: 8)
        }
        let current = context()
        let presentation = AtlasSearchPresentation(context: current, hits: [hit(0), hit(1), hit(2)], verifiedHitIDs: [])
        func move(_ ids: [Int], _ row: AtlasSearchRowID? = nil, backwards: Bool = false) -> AtlasSearchRowID? {
            presentation.adjacentRow(in: ids, after: row, backwards: backwards, context: current)
        }
        func row(_ id: Int) -> AtlasSearchRowID { presentation.rowID(for: id) }

        // Navigation preserves visible order, not dictionary or numeric ID order.
        precondition(move([2, 0, 1]) == row(2))
        precondition(move([2, 0, 1], backwards: true) == row(1))
        precondition(move([2, 0, 1], row(2)) == row(0))
        precondition(move([2, 0, 1], row(1)) == row(2))
        precondition(move([2, 0, 1], row(2), backwards: true) == row(1))
        precondition(move([1], row(1)) == row(1))
        precondition(move([]) == nil)

        // A scope change can remove the selection. Begin at the visible edge,
        // and never reintroduce a filtered-out candidate.
        precondition(move([1, 2], row(0)) == row(1))
        precondition(move([1, 2], row(0), backwards: true) == row(2))
        precondition(move([999, 2]) == row(2))

        // An old report's row 0 cannot alias row 0 of the new report.
        let old = AtlasSearchPresentation(context: current, hits: [hit(0)], verifiedHitIDs: [])
        precondition(move([0, 1], old.rowID(for: 0)) == row(0))
        precondition(presentation.adjacentRow(in: [0, 1], after: row(0), backwards: false,
            context: context("changed")) == nil)

        // Candidate selection permits verification-on-open, but not missing or
        // malformed capture witnesses, unsupported native paths or duplicate IDs.
        let guarded = AtlasSearchPresentation(context: current,
            hits: [hit(0, path: nil), hit(1, digest: nil), hit(2, digest: "bad"), hit(3), hit(4), hit(4)],
            verifiedHitIDs: [0, 1, 2, 4])
        let next = guarded.adjacentRow(in: [0, 1, 2, 4, 3], after: nil, backwards: false, context: current)
        precondition(next == guarded.rowID(for: 3))
        precondition(guarded.adjacentRow(in: [0, 1, 2, 4], after: nil, backwards: true, context: current) == nil)
        precondition(guarded.availability(for: guarded.rowID(for: 3), in: current).allowsActivation)

        // Exact search context compares bytes rather than Unicode equivalence.
        let unicode = AtlasSearchPresentation(context: context("é"), hits: [hit(0)], verifiedHitIDs: [])
        precondition(unicode.adjacentRow(in: [0], after: nil, backwards: false, context: context("e\u{301}")) == nil)
        precondition(unicode.adjacentRow(in: [0], after: nil, backwards: false, context: context("é")) == unicode.rowID(for: 0))
        print("PASS: 17 AtlasSearch visible-order/capture/context navigation assertions")
    }
}
