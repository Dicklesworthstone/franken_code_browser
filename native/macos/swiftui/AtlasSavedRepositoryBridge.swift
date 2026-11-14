import Foundation

@_silgen_name("fcb_saved_create") private func savedCreate() -> UInt64
@_silgen_name("fcb_saved_open") private func savedOpen(_ h: UInt64, _ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_info") private func savedInfo(_ h: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_members") private func savedMembers(_ h: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_attach_index") private func savedAttachIndex(_ h: UInt64, _ generation: UInt64, _ path: UnsafePointer<CChar>?, _ pin: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_detach_index") private func savedDetachIndex(_ h: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_build_index") private func savedBuildIndex(_ h: UInt64, _ operation: UInt64,
    _ destination: UnsafePointer<CChar>?, _ fileBytes: UInt64, _ sourceBytes: UInt64, _ grams: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_index_build_receipt") private func savedIndexBuildReceipt(_ h: UInt64, _ operation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_search") private func savedSearch(_ h: UInt64, _ generation: UInt64, _ needle: UnsafePointer<CChar>?, _ matches: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_search_counted") private func savedCountedSearch(_ h: UInt64, _ generation: UInt64, _ needle: UnsafePointer<CChar>?, _ matches: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_search_matching") private func savedMatchingSearch(_ h: UInt64, _ generation: UInt64,
    _ needle: UnsafePointer<CChar>?, _ caseSensitive: UInt64, _ countAllMatches: UInt64, _ matches: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_results") private func savedResults(_ h: UInt64, _ generation: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_clear_results") private func savedClearResults(_ h: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_open_hit_reader") private func savedHitReader(_ h: UInt64, _ reader: UInt64, _ generation: UInt64, _ hit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_expression_search") private func savedExpressionSearch(_ h: UInt64, _ generation: UInt64, _ query: UnsafePointer<CChar>?, _ matches: UInt64, _ scanBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_expression_search_counted") private func savedCountedExpressionSearch(_ h: UInt64, _ generation: UInt64, _ query: UnsafePointer<CChar>?, _ matches: UInt64, _ scanBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_expression_search_matching") private func savedMatchingExpressionSearch(_ h: UInt64, _ generation: UInt64,
    _ query: UnsafePointer<CChar>?, _ caseSensitive: UInt64, _ countAllMatches: UInt64,
    _ matches: UInt64, _ scanBytes: UInt64) -> UnsafeMutablePointer<CChar>?
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
@_silgen_name("fcb_saved_prepare_imports") private func savedPrepareImports(_ h: UInt64, _ generation: UInt64,
    _ imports: UInt64, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_imports") private func savedImports(_ h: UInt64, _ generation: UInt64,
    _ direction: UInt64, _ member: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_clear_imports") private func savedClearImports(_ h: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_open_import_reader") private func savedImportReader(_ h: UInt64, _ reader: UInt64,
    _ generation: UInt64, _ importID: UInt64, _ side: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_path_search") private func savedPathSearch(_ h: UInt64, _ generation: UInt64,
    _ query: UnsafePointer<UInt8>?, _ queryBytes: UInt64, _ mode: UInt64, _ matchCase: UInt64,
    _ results: UInt64, _ work: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_path_results") private func savedPathResults(_ h: UInt64, _ generation: UInt64,
    _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_clear_paths") private func savedClearPaths(_ h: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_open_path_hit_reader") private func savedPathReader(_ h: UInt64, _ reader: UInt64,
    _ generation: UInt64, _ hit: UInt64) -> UnsafeMutablePointer<CChar>?
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
        case .buildIndex(let operation, let destination, let limits):
            guard operation > 0, AtlasSavedIndexBuildRequest.validDestination(destination), limits.isValid else { return nil }
            return destination.withCString {
                take(savedBuildIndex(handle, operation, $0, limits.fileBytes, limits.sourceBytes, limits.grams))
            }
        case .indexBuildReceipt(let operation):
            guard operation > 0 else { return nil }
            return take(savedIndexBuildReceipt(handle, operation))
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
        case .matchingExpressionSearch(let generation, let query, let caseSensitive, let countAllMatches, let matches, let bytes):
            guard generation > 0, AtlasSavedSearchWire.validNeedle(query),
                  matches <= AtlasSavedSearchWire.maximumHits, countAllMatches || matches > 0,
                  bytes <= AtlasSavedSearchWire.maximumSourceBytes else { return nil }
            return query.withCString { take(savedMatchingExpressionSearch(handle, generation, $0,
                caseSensitive ? 1 : 0, countAllMatches ? 1 : 0, matches, bytes)) }
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
        case .prepareImports(let generation, let imports, let bytes):
            guard generation > 0, (1...AtlasSavedImportWire.maximumLinks).contains(imports),
                  bytes <= AtlasSavedImportWire.maximumSourceBytes else { return nil }
            return take(savedPrepareImports(handle, generation, imports, bytes))
        case .imports(let generation, let direction, let member, let start, let limit):
            guard generation > 0, (direction == .all) == (member == nil),
                  member == nil || member! < 65_536, start <= AtlasSavedImportWire.maximumLinks,
                  (1...128).contains(limit) else { return nil }
            return take(savedImports(handle, generation, direction.code, member ?? 0, start, limit))
        case .clearImports(let generation):
            guard generation > 0 else { return nil }
            return take(savedClearImports(handle, generation))
        case .openImport(let reader, let generation, let importID, let side):
            guard reader > 0, reader != handle, generation > 0,
                  (1...AtlasSavedImportWire.maximumLinks).contains(importID) else { return nil }
            return take(savedImportReader(handle, reader, generation, importID, side.code))
        case .pathSearch(let generation, let query, let mode, let caseSensitive, let results, let work):
            guard generation > 0, AtlasSavedPathWire.validQuery(query), results <= AtlasSavedPathWire.maximumResults,
                  work <= AtlasSavedPathWire.maximumWork else { return nil }
            return query.withUnsafeBufferPointer {
                take(savedPathSearch(handle, generation, $0.baseAddress, UInt64($0.count), mode.code,
                    caseSensitive ? 1 : 0, results, work))
            }
        case .pathResults(let generation, let start, let limit):
            guard generation > 0, start <= AtlasSavedPathWire.maximumResults, (1...128).contains(limit) else { return nil }
            return take(savedPathResults(handle, generation, start, limit))
        case .clearPaths(let generation):
            guard generation > 0 else { return nil }
            return take(savedClearPaths(handle, generation))
        case .openPathHit(let reader, let generation, let hit):
            guard reader > 0, reader != handle, generation > 0, (1...AtlasSavedPathWire.maximumResults).contains(hit) else { return nil }
            return take(savedPathReader(handle, reader, generation, hit))
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
