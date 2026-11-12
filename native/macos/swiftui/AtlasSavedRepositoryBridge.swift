import Foundation

@_silgen_name("fcb_saved_create") private func savedCreate() -> UInt64
@_silgen_name("fcb_saved_open") private func savedOpen(_ h: UInt64, _ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_info") private func savedInfo(_ h: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_members") private func savedMembers(_ h: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_attach_index") private func savedAttachIndex(_ h: UInt64, _ generation: UInt64, _ path: UnsafePointer<CChar>?, _ pin: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_detach_index") private func savedDetachIndex(_ h: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_search") private func savedSearch(_ h: UInt64, _ generation: UInt64, _ needle: UnsafePointer<CChar>?, _ matches: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_search_counted") private func savedCountedSearch(_ h: UInt64, _ generation: UInt64, _ needle: UnsafePointer<CChar>?, _ matches: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_search_matching") private func savedMatchingSearch(_ h: UInt64, _ generation: UInt64,
    _ needle: UnsafePointer<CChar>?, _ caseSensitive: UInt64, _ countAllMatches: UInt64, _ matches: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_results") private func savedResults(_ h: UInt64, _ generation: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_clear_results") private func savedClearResults(_ h: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_open_hit_reader") private func savedHitReader(_ h: UInt64, _ reader: UInt64, _ generation: UInt64, _ hit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_expression_search") private func savedExpressionSearch(_ h: UInt64, _ generation: UInt64, _ query: UnsafePointer<CChar>?, _ matches: UInt64, _ scanBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_expression_search_counted") private func savedCountedExpressionSearch(_ h: UInt64, _ generation: UInt64, _ query: UnsafePointer<CChar>?, _ matches: UInt64, _ scanBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_expression_results") private func savedExpressionResults(_ h: UInt64, _ generation: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_clear_expression") private func savedClearExpression(_ h: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_open_expression_hit_reader") private func savedExpressionReader(_ h: UInt64, _ reader: UInt64, _ generation: UInt64, _ hit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_structure_search") private func savedStructureSearch(_ h: UInt64, _ generation: UInt64,
    _ mode: UInt64, _ nameMode: UInt64, _ query: UnsafePointer<CChar>?, _ matches: UInt64, _ sourceBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_structure_results") private func savedStructureResults(_ h: UInt64, _ generation: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_clear_structure") private func savedClearStructure(_ h: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_open_structure_hit_reader") private func savedStructureReader(_ h: UInt64, _ reader: UInt64, _ generation: UInt64, _ hit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_prepare_relationships") private func savedPrepare(_ h: UInt64, _ generation: UInt64, _ links: UInt64, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_relationships") private func savedRelationships(_ h: UInt64, _ generation: UInt64,
    _ direction: UInt64, _ member: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_clear_relationships") private func savedClear(_ h: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_open_member_reader") private func savedMemberReader(_ h: UInt64, _ reader: UInt64, _ member: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_open_relationship_reader") private func savedLinkReader(_ h: UInt64, _ reader: UInt64,
    _ generation: UInt64, _ link: UInt64, _ side: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_document_images_authorize") private func savedImagesAuthorize(_ h: UInt64,
    _ reader: UInt64, _ member: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_document_image_prepare") private func savedImagePrepare(_ h: UInt64,
    _ reader: UInt64, _ member: UInt64, _ generation: UInt64,
    _ item: UInt64, _ request: UInt64) -> UnsafeMutablePointer<CChar>?
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
        case .attachIndex(let generation, let path, let pin):
            guard generation > 0, path.hasPrefix("/"), path.utf8.count <= 16_384,
                  !path.utf8.contains(0), AtlasSavedWire.digest(pin) else { return nil }
            return path.withCString { path in pin.withCString { take(savedAttachIndex(handle, generation, path, $0)) } }
        case .detachIndex(let generation):
            guard generation > 0 else { return nil }
            return take(savedDetachIndex(handle, generation))
        case .search(let generation, let needle, let matches):
            guard generation > 0, AtlasSavedSearchWire.validNeedle(needle),
                  (1...AtlasSavedSearchWire.maximumHits).contains(matches) else { return nil }
            return needle.withCString { take(savedSearch(handle, generation, $0, matches)) }
        case .countedSearch(let generation, let needle, let matches):
            guard generation > 0, AtlasSavedSearchWire.validNeedle(needle),
                  matches <= AtlasSavedSearchWire.maximumHits else { return nil }
            return needle.withCString { take(savedCountedSearch(handle, generation, $0, matches)) }
        case .matchingSearch(let generation, let needle, let caseSensitive, let countAllMatches, let matches):
            guard generation > 0, AtlasSavedSearchWire.validNeedle(needle),
                  matches <= AtlasSavedSearchWire.maximumHits, countAllMatches || matches > 0 else { return nil }
            return needle.withCString { take(savedMatchingSearch(handle, generation, $0,
                caseSensitive ? 1 : 0, countAllMatches ? 1 : 0, matches)) }
        case .results(let generation, let start, let limit):
            return take(savedResults(handle, generation, start, limit))
        case .clearResults(let generation): return take(savedClearResults(handle, generation))
        case .openHit(let reader, let generation, let hit): return take(savedHitReader(handle, reader, generation, hit))
        case .expressionSearch(let generation, let query, let matches, let bytes):
            guard generation > 0, AtlasSavedSearchWire.validNeedle(query),
                  (1...AtlasSavedSearchWire.maximumHits).contains(matches),
                  bytes <= AtlasSavedSearchWire.maximumSourceBytes else { return nil }
            return query.withCString { take(savedExpressionSearch(handle, generation, $0, matches, bytes)) }
        case .countedExpressionSearch(let generation, let query, let matches, let bytes):
            guard generation > 0, AtlasSavedSearchWire.validNeedle(query),
                  matches <= AtlasSavedSearchWire.maximumHits,
                  bytes <= AtlasSavedSearchWire.maximumSourceBytes else { return nil }
            return query.withCString { take(savedCountedExpressionSearch(handle, generation, $0, matches, bytes)) }
        case .expressionResults(let generation, let start, let limit):
            return take(savedExpressionResults(handle, generation, start, limit))
        case .clearExpression(let generation): return take(savedClearExpression(handle, generation))
        case .openExpressionHit(let reader, let generation, let hit):
            return take(savedExpressionReader(handle, reader, generation, hit))
        case .structureSearch(let generation, let mode, let nameMode, let query, let matches, let bytes):
            guard generation > 0, let structure = mode.structure, AtlasSavedSearchWire.validNeedle(query),
                  mode != .occurrences || nameMode == .exact,
                  (1...AtlasSavedSearchWire.maximumHits).contains(matches),
                  bytes <= AtlasSavedSearchWire.maximumSourceBytes else { return nil }
            return query.withCString { take(savedStructureSearch(handle, generation, structure.code, nameMode.code, $0, matches, bytes)) }
        case .structureResults(let generation, let start, let limit):
            return take(savedStructureResults(handle, generation, start, limit))
        case .clearStructure(let generation): return take(savedClearStructure(handle, generation))
        case .openStructureHit(let reader, let generation, let hit):
            return take(savedStructureReader(handle, reader, generation, hit))
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
    }, readerDomain: AtlasReaderTransport.native.comparison?.domain,
        imagesAuthorize: { handle, reader, member, generation in
            guard handle > 0, reader > 0, handle != reader, generation > 0 else { return nil }
            return take(savedImagesAuthorize(handle, reader, member, generation))
        }, imagePrepare: { handle, reader, member, generation, item, request in
            guard handle > 0, reader > 0, handle != reader, generation > 0 else { return nil }
            return take(savedImagePrepare(handle, reader, member, generation, item, request))
        })

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { savedFree(pointer) }
        var count = 0
        while count <= AtlasSavedWire.maximumResponseBytes && pointer[count] != 0 { count += 1 }
        guard count > 0, count <= AtlasSavedWire.maximumResponseBytes else { return nil }
        return String(data: Data(bytes: pointer, count: count), encoding: .utf8)
    }
}
