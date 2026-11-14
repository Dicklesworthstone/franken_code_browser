import Foundation

@main struct AtlasRefreshLayoutTests {
    static func entry(_ path: String, id: UInt64 = 1, bytes: UInt64 = 100,
        x: Double = 0, y: Double = 0, width: Double = 80, height: Double = 60) -> AtlasProjectCatalog.Entry {
        AtlasProjectCatalog.Entry(id: id, rawPath: Array(path.utf8), displayPath: path,
            sourcePath: path, observedBytes: bytes, x: x, y: y, width: width, height: height)
    }
    static func main() {
        let a = entry("src/a.rs", x: 10, y: 20), b = entry("src/b.rs", id: 2, x: 130, y: 20)
        let freshA = entry("src/a.rs", id: 8, bytes: 9_999, x: 900), freshB = entry("src/b.rs", id: 1, bytes: 7)
        let c = entry("new/c.rs", id: 2, bytes: 900)
        var packed: [[UInt8]] = []
        let result = AtlasRefreshLayout.reconcile(previous: [a, b], observed: [freshB, c, freshA], complete: true) {
            packed = $0.map(\.rawPath)
            return $0
        }!
        precondition(packed == [c.rawPath], "only additions go through the existing packer")
        let keptA = result.entries.first { $0.rawPath == a.rawPath }!
        let keptB = result.entries.first { $0.rawPath == b.rawPath }!
        let addedC = result.entries.first { $0.rawPath == c.rawPath }!
        precondition(keptA.x == a.x && keptA.y == a.y && keptA.width == a.width && keptA.height == a.height)
        precondition(keptB.x == b.x && keptB.y == b.y && keptB.width == b.width && keptB.height == b.height)
        precondition(keptA.observedBytes == 9_999 && keptB.observedBytes == 7,
            "metadata changes independently of established parcel geometry")
        precondition(addedC.x >= b.x + b.width + 32)
        precondition(result.addedRawPaths == [c.rawPath] && result.removedCount == 0 && result.unconfirmedRawPaths.isEmpty)
        precondition(Set(result.entries.map(\.id)).count == 3, "response-local row reuse cannot alias paths")

        let partial = AtlasRefreshLayout.reconcile(previous: [a, b], observed: [freshA, c], complete: false) { $0 }!
        precondition(partial.entries.count == 3 && partial.unconfirmedRawPaths == [b.rawPath] && partial.removedCount == 0)
        precondition(partial.entries.first { $0.rawPath == b.rawPath }?.observedBytes == b.observedBytes)
        let emptyPartial = AtlasRefreshLayout.reconcile(previous: [a, b], observed: [], complete: false) {
            _ in preconditionFailure("no additions need packing")
        }!
        precondition(emptyPartial.entries.count == 2 && emptyPartial.unconfirmedRawPaths == [a.rawPath, b.rawPath])
        let complete = AtlasRefreshLayout.reconcile(previous: [a, b], observed: [freshA], complete: true) {
            _ in preconditionFailure("size changes must not repack")
        }!
        precondition(complete.entries.count == 1 && complete.removedCount == 1 && complete.unconfirmedRawPaths.isEmpty)
        let next = AtlasRefreshLayout.reconcile(previous: complete.entries, observed: [freshA, c], complete: true,
            reservedRightEdge: b.x + b.width, pack: { $0 })!
        precondition(next.entries.first { $0.rawPath == c.rawPath }!.x >= b.x + b.width + 32,
            "a later refresh cannot reuse the removed edge parcel's remembered coordinate frame")
        let cleared = AtlasRefreshLayout.reconcile(previous: [a, b], observed: [], complete: true, pack: { $0 })!
        precondition(cleared.entries.isEmpty && cleared.removedCount == 2)

        let composed = entry("é.rs"), decomposed = entry("e\u{301}.rs", id: 2, x: 100)
        let unicode = AtlasRefreshLayout.reconcile(previous: [composed, decomposed],
            observed: [entry("é.rs", bytes: 8)], complete: false, pack: { $0 })!
        precondition(unicode.entries.count == 2 && unicode.unconfirmedRawPaths == [decomposed.rawPath],
            "canonical-equivalent Swift strings remain distinct Unix paths")
        let raw = AtlasProjectCatalog.Entry(id: 5, rawPath: [255], displayPath: "\\xFF", sourcePath: nil,
            observedBytes: 12, x: 500, y: 0, width: 60, height: 50)
        let unsupported = AtlasRefreshLayout.reconcile(previous: [raw], observed: [], complete: false, pack: { $0 })!
        precondition(unsupported.entries.first?.rawPath == [255] && unsupported.unconfirmedRawPaths == [[255]])

        precondition(AtlasRefreshLayout.reconcile(previous: [a, a], observed: [], complete: false, pack: { $0 }) == nil)
        precondition(AtlasRefreshLayout.reconcile(previous: [], observed: [a, a], complete: true, pack: { $0 }) == nil)
        precondition(AtlasRefreshLayout.reconcile(previous: [a], observed: [a, c], complete: true, pack: { _ in [] }) == nil)
        precondition(AtlasRefreshLayout.reconcile(previous: [a], observed: [a, c], complete: true,
            pack: { _ in [entry("forged.rs")] }) == nil)
        precondition(AtlasRefreshLayout.reconcile(previous: [a], observed: [a, c], complete: true,
            reservedRightEdge: 1e12, pack: { $0 }) == nil, "coordinate envelope growth stays bounded")
        precondition(AtlasRefreshLayout.reconcile(previous: [a], observed: [a, c], complete: true,
            reservedRightEdge: .nan, pack: { $0 }) == nil)
        let old = (0..<10_000).map { entry("old/\($0)", id: UInt64($0 + 1)) }
        let incoming = (0..<10_001).map { entry("new/\($0)", id: UInt64($0 + 1)) }
        precondition(AtlasRefreshLayout.reconcile(previous: old, observed: incoming, complete: false,
            pack: { _ in preconditionFailure("over-budget unions are refused before layout") }) == nil,
            "partial discovery cannot silently discard old members to make room")
        print("Refresh parcel continuity, additions, coverage, raw identity and admission bounds passed.")
    }
}
