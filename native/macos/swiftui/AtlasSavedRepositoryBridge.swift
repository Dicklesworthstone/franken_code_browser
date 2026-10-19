import Foundation

@_silgen_name("fcb_saved_create") private func savedCreate() -> UInt64
@_silgen_name("fcb_saved_open") private func savedOpen(_ h: UInt64, _ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_info") private func savedInfo(_ h: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_members") private func savedMembers(_ h: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_prepare_relationships") private func savedPrepare(_ h: UInt64, _ generation: UInt64, _ links: UInt64, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_relationships") private func savedRelationships(_ h: UInt64, _ generation: UInt64,
    _ direction: UInt64, _ member: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_clear_relationships") private func savedClear(_ h: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_open_member_reader") private func savedMemberReader(_ h: UInt64, _ reader: UInt64, _ member: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_open_relationship_reader") private func savedLinkReader(_ h: UInt64, _ reader: UInt64,
    _ generation: UInt64, _ link: UInt64, _ side: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_cancel") private func savedCancel(_ h: UInt64) -> UInt8
@_silgen_name("fcb_saved_close") private func savedClose(_ h: UInt64) -> UInt8
@_silgen_name("fcb_free_string") private func savedFree(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasSavedTransport {
    static let native = Self(create: { savedCreate() }, run: { handle, request in
        switch request {
        case .open(let path):
            guard path.hasPrefix("/"), path.utf8.count <= 16_384, !path.utf8.contains(0) else { return nil }
            return path.withCString { take(savedOpen(handle, $0)) }
        case .info: return take(savedInfo(handle))
        case .members(let start, let limit): return take(savedMembers(handle, start, limit))
        case .prepareRelationships(let generation, let links, let bytes):
            return take(savedPrepare(handle, generation, links, bytes))
        case .relationships(let generation, let direction, let member, let start, let limit):
            guard (direction == .all) == (member == nil) else { return nil }
            return take(savedRelationships(handle, generation, direction.code, member ?? 0, start, limit))
        case .clearRelationships(let generation): return take(savedClear(handle, generation))
        case .openMember(let reader, let member): return take(savedMemberReader(handle, reader, member))
        case .openRelationship(let reader, let generation, let link, let side):
            return take(savedLinkReader(handle, reader, generation, link, side.code))
        }
    }, cancel: { savedCancel($0) == 1 }, close: { savedClose($0) == 1 }, retirementFailed: {
        try? FileHandle.standardError.write(contentsOf: Data("FCB_SAVED_RETIREMENT_UNAVAILABLE\n".utf8))
    }, readerDomain: AtlasReaderTransport.native.comparison?.domain)

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { savedFree(pointer) }
        var count = 0
        while count <= AtlasSavedWire.maximumResponseBytes && pointer[count] != 0 { count += 1 }
        guard count > 0, count <= AtlasSavedWire.maximumResponseBytes else { return nil }
        return String(data: Data(bytes: pointer, count: count), encoding: .utf8)
    }
}
