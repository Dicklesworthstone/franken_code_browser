import Foundation

/// Link with the actual fcb bridge. Source/cache calls are production operations;
/// neither these checks nor the queue tests qualify a physical Mac window.
@_silgen_name("fcb_source_cache_open") private func openCache(_ path: UnsafePointer<CChar>) -> UInt64
@_silgen_name("fcb_source_cache_close") private func closeCache(_ handle: UInt64) -> Bool
@main struct AtlasProjectWorkerTests {
    static func main() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("fcb-project-worker-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        let source = root.appendingPathComponent("source.rs")
        let original = "\u{feff}fn main() { /* 😀 */ }\r\n"
        try Data(original.utf8).write(to: source)
        let token = AtlasSearchCancellation()
        let catalog = try AtlasProjectWorker.catalog(root: root.path, cancellation: token)
        precondition(catalog.count == 1 && catalog[0].path == "source.rs")
        precondition(catalog[0].bytes == original.utf8.count && catalog[0].profile.isEmpty)
        precondition(catalog[0].profileState == "disabled")
        let packet = try AtlasProjectWorker.source(path: source.path, cancellation: token)!
        let object = try JSONSerialization.jsonObject(with: packet.json) as! [String: Any]
        precondition((object["text"] as! String).utf8.elementsEqual(original.utf8))
        precondition(packet.key == nil)
        let cachePath = root.appendingPathComponent("cache").path
        let handle = cachePath.withCString { openCache($0) }; precondition(handle != 0)
        defer { precondition(closeCache(handle)) }
        let cached = try AtlasProjectWorker.source(path: source.path, handle: handle, cancellation: token)!
        precondition(cached.json == packet.json && cached.key?.count == 32)
        let again = try AtlasProjectWorker.source(path: source.path, handle: handle, cancellation: token)!
        precondition(cached.key == again.key && cached.json == again.json)
        let missing = try AtlasProjectWorker.source(path: root.appendingPathComponent("missing").path, cancellation: token)
        precondition(missing == nil)
        let empty = root.appendingPathComponent("empty.rs"); try Data().write(to: empty)
        let emptyPacket = try AtlasProjectWorker.source(path: empty.path, cancellation: token)!
        let emptyObject = try JSONSerialization.jsonObject(with: emptyPacket.json) as! [String: Any]
        precondition((emptyObject["text"] as! String).isEmpty)
        token.cancel()
        do { _ = try AtlasProjectWorker.catalog(root: root.path, cancellation: token); preconditionFailure("canceled catalog accepted") }
        catch AtlasProjectIOError.canceled { }
        do { _ = try AtlasProjectWorker.source(path: source.path, handle: handle, cancellation: token); preconditionFailure("canceled source accepted") }
        catch AtlasProjectIOError.canceled { }
        for text in ["{}", "{\"files\":[{}]}", "{\"files\":null}"] {
            do { _ = try AtlasProjectWorker.decodeCatalog(Data(text.utf8)); preconditionFailure("invalid catalog accepted") }
            catch AtlasProjectIOError.invalidResponse { }
        }
        let emptyCatalog = try AtlasProjectWorker.decodeCatalog(Data("{\"files\":[]}".utf8))
        precondition(emptyCatalog.isEmpty)
        print("AtlasProjectWorker: production catalog/source/cache, cancellation and decoding checks passed")
    }
}
