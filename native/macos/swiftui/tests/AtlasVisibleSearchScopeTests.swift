import Foundation

@main private enum AtlasVisibleSearchScopeTests {
    private static func entry(_ id: UInt64, _ raw: [UInt8], x: Double, y: Double = 0,
        width: Double = 10, height: Double = 10, bytes: UInt64 = 32) -> AtlasProjectCatalog.Entry {
        .init(id: id, rawPath: raw, displayPath: "presentation only", sourcePath: String(bytes: raw, encoding: .utf8),
            observedBytes: bytes, x: x, y: y, width: width, height: height)
    }
    private static func view(x: Double = 0, y: Double = 0, width: Double = 100, height: Double = 100,
        root: String = "/repo", project: UUID = UUID(), layout: UUID = UUID()) throws -> AtlasVisibleSearchScope.View {
        try .init(root: root, project: project, layout: layout,
            viewport: .init(x: x, y: y, width: width, height: height))
    }
    private static func capture(_ entries: [AtlasProjectCatalog.Entry], in view: AtlasVisibleSearchScope.View,
        files: Int = 20_000, wireBytes: Int = 1_048_576, pathBytes: Int = 4096) throws -> AtlasVisibleSearchScope {
        try .capture(view: view, entries: entries, maximumFiles: files,
            maximumWireBytes: wireBytes, maximumPathBytes: pathBytes)
    }
    private static func reject(_ message: String, _ operation: () throws -> Void) {
        do { try operation(); preconditionFailure(message) } catch { }
    }

    static func main() throws {
        try geometryUsesAllParcelsAndExactIntersections()
        try rawMembershipIsFrozenAndNotAPathLabel()
        try boundsRefuseWholeScopeAndEmptyRemainsEmpty()
        try invalidGeometryAndCancellationCannotPublishMembership()
        print("Accepted atlas-view membership, exact raw paths, frozen scope identity, bounded refusal and cancellation checks passed.")
    }

    private static func geometryUsesAllParcelsAndExactIntersections() throws {
        let accepted = try view()
        let entries = [
            entry(1, Array("large.rs".utf8), x: 5, bytes: 100 * 1024 * 1024),
            entry(2, Array("tiny.rs".utf8), x: 50, width: 0.0001, height: 0.0001),
            entry(3, [255, 46, 114, 115], x: 95, width: 10),
            entry(4, Array("right-edge.rs".utf8), x: 100),
            entry(5, Array("bottom-edge.rs".utf8), x: 10, y: 100),
            entry(6, Array("left-edge.rs".utf8), x: -10),
            entry(7, Array("clipped.rs".utf8), x: -5),
            entry(8, Array("outside.rs".utf8), x: 200)
        ]
        let frozen = try capture(entries, in: accepted)
        precondition(frozen.paths == [entries[0].rawPath, entries[1].rawPath, entries[2].rawPath, entries[6].rawPath])
        precondition(frozen.count == 4 && frozen.view.id == accepted.id)
        // These parcels require no source payload or native text preview. A
        // reader-only large source and raw non-UTF-8 filename both participate.
        precondition(entries[2].sourcePath == nil && frozen.paths.contains(entries[2].rawPath))
    }

    private static func rawMembershipIsFrozenAndNotAPathLabel() throws {
        let project = UUID(), layout = UUID()
        let accepted = try view(root: "/répo", project: project, layout: layout)
        let first = Array("café.rs".utf8), second = Array("cafe\u{301}.rs".utf8)
        let entries = [entry(1, first, x: 0), entry(2, second, x: 20), entry(3, first, x: 40)]
        let frozen = try capture(entries, in: accepted)
        precondition(frozen.paths == [first, second] && frozen.count == 2)
        precondition(String(decoding: first, as: UTF8.self) == String(decoding: second, as: UTF8.self))
        precondition(frozen.view.belongs(to: "/répo", project: project, layout: layout))
        precondition(!frozen.view.belongs(to: "/re\u{301}po", project: project, layout: layout))
        precondition(!frozen.view.belongs(to: "/répo", project: UUID(), layout: layout))
        precondition(!frozen.view.belongs(to: "/répo", project: project, layout: UUID()))
        let moved = try view(x: 200, root: "/répo", project: project, layout: layout)
        precondition(!frozen.view.represents(root: "/répo", project: project, layout: layout, viewport: moved.viewport))
        precondition(frozen.view.viewport == accepted.viewport && frozen.paths == [first, second])
        let refreshed = try capture(entries, in: moved)
        precondition(refreshed.paths.isEmpty && refreshed.id != frozen.id && refreshed.view.id != frozen.view.id)
    }

    private static func boundsRefuseWholeScopeAndEmptyRemainsEmpty() throws {
        let accepted = try view()
        let entries = [entry(1, Array("a".utf8), x: 0), entry(2, Array("bc".utf8), x: 20)]
        let exact = try capture(entries, in: accepted, files: 2, wireBytes: 7)
        precondition(exact.count == 2 && exact.encodedPathBytes == 7)
        reject("oversized membership was silently truncated") { _ = try capture(entries, in: accepted, files: 1) }
        reject("oversized manifest was partially accepted") { _ = try capture(entries, in: accepted, wireBytes: 6) }
        reject("oversized filename entered the manifest") { _ = try capture(entries, in: accepted, pathBytes: 1) }
        let duplicate = try capture([entries[0], entries[0]], in: accepted, files: 1, wireBytes: 2)
        precondition(duplicate.count == 1 && duplicate.encodedPathBytes == 2)
        let empty = try capture([], in: accepted)
        precondition(empty.paths.isEmpty && empty.count == 0 && empty.encodedPathBytes == 0)
        let offscreen = try capture([entry(1, Array("elsewhere.rs".utf8), x: 200)], in: accepted)
        precondition(offscreen.paths.isEmpty && offscreen.encodedPathBytes == 0)
    }

    private static func invalidGeometryAndCancellationCannotPublishMembership() throws {
        for values in [(Double.nan, 0.0, 10.0, 10.0), (0.0, 0.0, Double.infinity, 10.0),
                       (0.0, 0.0, 0.0, 10.0), (0.0, 0.0, 10.0, -1.0), (1e13, 0.0, 10.0, 10.0)] {
            reject("invalid viewport was accepted") { _ = try view(x: values.0, y: values.1, width: values.2, height: values.3) }
        }
        let accepted = try view()
        reject("invalid parcel geometry produced a partial scope") {
            _ = try capture([entry(1, Array("a".utf8), x: .nan)], in: accepted)
        }
        for raw in [[], [47, 97], [97, 0], Array("../a".utf8), Array("a//b".utf8), Array("a/./b".utf8)] {
            reject("non-normalized visible path was accepted") { _ = try capture([entry(1, raw, x: 0)], in: accepted) }
        }
        let sample = entry(1, Array("a".utf8), x: 0)
        reject("unbounded metadata input was accepted") {
            _ = try capture(Array(repeating: sample, count: 20_001), in: accepted)
        }
        final class Cancellation: @unchecked Sendable {
            private let lock = NSLock()
            private var count = 0
            func check() -> Bool {
                lock.lock(); defer { lock.unlock() }
                count += 1; return count >= 3
            }
        }
        let cancellation = Cancellation()
        reject("mid-freeze cancellation published a partial scope") {
            _ = try AtlasVisibleSearchScope.capture(view: accepted, entries: [sample, sample],
                maximumFiles: 20_000, maximumWireBytes: 1_048_576, maximumPathBytes: 4096,
                canceled: { cancellation.check() })
        }
    }
}
