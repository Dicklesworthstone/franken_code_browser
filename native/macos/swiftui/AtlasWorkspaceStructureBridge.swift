import Foundation

struct AtlasWorkspaceStructureTransport: Sendable {
    let create: @Sendable () -> UInt64
    let open: @Sendable (UInt64, String, UInt64, AtlasWorkspaceStructureRequest) throws -> String?
    let advance: @Sendable (UInt64, UInt64, UInt64, UInt64) throws -> String?
    let page: @Sendable (UInt64, UInt64, UInt64, UInt64, UInt64) throws -> String?
    let stop: @Sendable (UInt64, UInt64) throws -> String?
    let openReader: @Sendable (UInt64, UInt64, UInt64, UInt64, UInt64) throws -> String?
    let cancel: @Sendable (UInt64) -> Bool
    let close: @Sendable (UInt64) -> Bool
    let retirementFailed: @Sendable () -> Void
}

@_silgen_name("fcb_catalog_structure_create") private func structureCreate() -> UInt64
@_silgen_name("fcb_catalog_structure_open_with_policy") private func structureOpen(
    _ owner: UInt64, _ root: UnsafePointer<CChar>?, _ generation: UInt64,
    _ query: UnsafePointer<UInt8>?, _ queryBytes: UInt64, _ mode: UInt64, _ nameMode: UInt64,
    _ startAfter: UInt64, _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?, _ maximumMatches: UInt64,
    _ maximumBytes: UInt64, _ maximumCalls: UInt64, _ policy: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_structure_advance") private func structureAdvance(
    _ owner: UInt64, _ generation: UInt64, _ revision: UInt64, _ steps: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_structure_page") private func structurePage(
    _ owner: UInt64, _ generation: UInt64, _ revision: UInt64, _ offset: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_structure_stop") private func structureStop(
    _ owner: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_structure_open_hit_reader") private func structureOpenReader(
    _ owner: UInt64, _ reader: UInt64, _ generation: UInt64, _ revision: UInt64, _ index: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_structure_cancel") private func structureCancel(_ owner: UInt64) -> UInt8
@_silgen_name("fcb_catalog_structure_close") private func structureClose(_ owner: UInt64) -> UInt8
@_silgen_name("fcb_free_string") private func structureFree(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasWorkspaceStructureTransport {
    static let native = Self(create: { structureCreate() },
        open: { owner, root, generation, request in
            guard owner > 0, generation > 0, root.utf8.first == 47,
                  root.utf8.count <= 16_384, !root.utf8.contains(0) else {
                throw AtlasWorkspaceStructureError.invalidInput
            }
            let bytes = Array(request.needle.utf8)
            return try root.withCString { root in
                try bytes.withUnsafeBufferPointer { query in
                    try request.fileScope.withCStrings { extensions, filenames in
                        try request.pathScope.withCString { kind, path in
                            try structureTake(structureOpen(owner, root, generation, query.baseAddress,
                                UInt64(query.count), request.mode.code, request.nameMode.code, request.startAfter,
                                extensions, filenames, kind, path, request.maximumMatches,
                                request.maximumScanBytes, request.maximumReadCalls, request.discoveryPolicy.rawValue))
                        }
                    }
                }
            }
        },
        advance: { try structureTake(structureAdvance($0, $1, $2, $3)) },
        page: { try structureTake(structurePage($0, $1, $2, $3, $4)) },
        stop: { try structureTake(structureStop($0, $1)) },
        openReader: { try structureTake(structureOpenReader($0, $1, $2, $3, $4)) },
        cancel: { structureCancel($0) == 1 }, close: { structureClose($0) == 1 },
        retirementFailed: { NSLog("Progressive structure-search retirement could not be confirmed") })
}

private func structureTake(_ pointer: UnsafeMutablePointer<CChar>?) throws -> String? {
    guard let pointer else { return nil }
    defer { structureFree(pointer) }
    let length = strnlen(pointer, AtlasWorkspaceStructureLimits.responseBytes + 1)
    guard length > 0, length <= AtlasWorkspaceStructureLimits.responseBytes,
          let text = String(bytes: UnsafeBufferPointer(start: UnsafeRawPointer(pointer).assumingMemoryBound(to: UInt8.self),
            count: length), encoding: .utf8) else { throw AtlasWorkspaceStructureError.invalidResponse }
    return text
}

