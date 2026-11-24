import Foundation

@_silgen_name("fcb_catalog_stream_create") private func catalogStreamCreate() -> UInt64
@_silgen_name("fcb_catalog_stream_open_with_policy") private func catalogStreamOpenWithPolicy(
    _ owner: UInt64, _ root: UnsafePointer<CChar>?, _ generation: UInt64,
    _ query: UnsafePointer<UInt8>?, _ queryBytes: UInt64, _ mode: UInt64,
    _ caseSensitive: UInt64, _ countAll: UInt64, _ startAfter: UInt64,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?, _ maximumMatches: UInt64,
    _ maximumBytes: UInt64, _ maximumCalls: UInt64, _ policy: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_stream_advance") private func catalogStreamAdvance(
    _ owner: UInt64, _ generation: UInt64, _ revision: UInt64, _ steps: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_stream_page") private func catalogStreamPage(
    _ owner: UInt64, _ generation: UInt64, _ revision: UInt64,
    _ offset: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_stream_hit") private func catalogStreamHit(
    _ owner: UInt64, _ generation: UInt64, _ revision: UInt64, _ index: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_stream_stop") private func catalogStreamStop(
    _ owner: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_stream_cancel") private func catalogStreamCancel(_ owner: UInt64) -> UInt8
@_silgen_name("fcb_catalog_stream_close") private func catalogStreamClose(_ owner: UInt64) -> UInt8
@_silgen_name("fcb_free_string") private func catalogStreamFree(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasWorkspaceStreamTransport {
    /// Reuses the verified search, witness and observed-reader model. Unlike
    /// the atlas transport, each query owns its progressive discovery cursor.
    static let catalogNative = Self(create: { catalogStreamCreate() },
        open: { _, _, _ in throw AtlasWorkspaceStreamError.unavailable },
        search: { _, _, _ in throw AtlasWorkspaceStreamError.unavailable },
        page: { _, _, _, _ in throw AtlasWorkspaceStreamError.unavailable },
        hit: { _, _, _ in throw AtlasWorkspaceStreamError.unavailable },
        clear: { _ in throw AtlasWorkspaceStreamError.unavailable },
        cancel: { catalogStreamCancel($0) != 0 }, close: { catalogStreamClose($0) != 0 },
        retirementFailed: { NSLog("Progressive project search retirement could not be confirmed") },
        catalogSearch: .init(search: { owner, root, generation, request, flag, stop, progress in
            try catalogStreamSearch(owner: owner, root: root, generation: generation,
                request: request, flag: flag, stop: stop, progress: progress)
        }, page: { try catalogStreamTake(catalogStreamPage($0, $1, $2, $3, $4)) },
            hit: { try catalogStreamTake(catalogStreamHit($0, $1, $2, $3)) }), supportsDiscoveryPolicy: true, domain: nativeDomain)
}

/// Scheduling and bounded receipt copying only. Traversal, matching, scope
/// admission, byte ranges and retained witnesses remain owned by the engine.
private func catalogStreamSearch(owner: UInt64, root: String, generation: UInt64,
    request: AtlasWorkspaceStreamRequest, flag: AtlasSearchCancellation,
    stop: AtlasWorkspaceStreamStop, progress: @Sendable (String) -> Void) throws -> String? {
    guard !flag.isCanceled else { throw AtlasWorkspaceStreamError.canceled }
    defer { if flag.isCanceled { _ = catalogStreamCancel(owner) } }
    let mode: UInt64 = request.mode == .text ? 0 : request.mode == .occurrences ? 1 : 2
    let bytes = Array(request.needle.utf8)
    var response = try root.withCString { root in
        try bytes.withUnsafeBufferPointer { query in
            try request.fileScope.withCStrings { extensions, filenames in
                try request.pathScope.withCString { kind, path in
                    try catalogStreamTake(catalogStreamOpenWithPolicy(owner, root, generation,
                        query.baseAddress, UInt64(query.count), mode, request.caseSensitive ? 1 : 0,
                        request.countAllMatches ? 1 : 0, request.pageStartAfter, extensions, filenames,
                        kind, path, request.maximumMatches, request.maximumScanBytes, request.maximumReadCalls,
                        request.discoveryPolicy.rawValue))
                }
            }
        }
    }
    var command = "info", priorRevision: UInt64 = 0
    while true {
        guard !flag.isCanceled else { throw AtlasWorkspaceStreamError.canceled }
        let wire = try AtlasWorkspaceStreamWire.read(response, owner: owner)
        try wire.header(schema: "fcb.catalog-stream/1", command: command, owner: owner)
        try wire.discoveryPolicy(request: request, progressive: true)
        let revision = try wire.number("revision"), more = try wire.flag("more")
        guard revision > priorRevision, try wire.number("progress_sequence") == revision,
              try wire.number("query_generation") == generation else {
            throw AtlasWorkspaceStreamError.invalidResponse
        }
        try wire.terminalState(expected: !more, required: true)
        if !more {
            let page = try catalogStreamTake(catalogStreamPage(owner, generation, revision, 0, AtlasWorkspaceStreamLimits.page))
            guard !flag.isCanceled else { throw AtlasWorkspaceStreamError.canceled }
            return page
        }
        guard let progressResponse = response else { throw AtlasWorkspaceStreamError.invalidResponse }
        progress(progressResponse)
        guard !flag.isCanceled else { throw AtlasWorkspaceStreamError.canceled }
        priorRevision = revision
        if stop.isRequested {
            command = "stop"
            response = try catalogStreamTake(catalogStreamStop(owner, generation))
        } else {
            command = "advance"
            // Sixteen bounded steps yield progress and a cancellation/stop
            // check after at most 256 KiB of new source input.
            response = try catalogStreamTake(catalogStreamAdvance(owner, generation, revision, 16))
        }
    }
}

private func catalogStreamTake(_ pointer: UnsafeMutablePointer<CChar>?) throws -> String? {
    guard let pointer else { return nil }
    defer { catalogStreamFree(pointer) }
    let count = strnlen(pointer, AtlasWorkspaceStreamLimits.responseBytes + 1)
    guard count <= AtlasWorkspaceStreamLimits.responseBytes,
          let text = String(bytes: UnsafeBufferPointer(start: UnsafeRawPointer(pointer).assumingMemoryBound(to: UInt8.self),
            count: count), encoding: .utf8) else { throw AtlasWorkspaceStreamError.invalidResponse }
    return text
}
