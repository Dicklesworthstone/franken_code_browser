import Foundation

private typealias WorkspaceStreamPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
@_silgen_name("fcb_atlas_create") private func workspaceStreamCreate() -> UInt64
@_silgen_name("fcb_atlas_open_cancelable") private func workspaceStreamOpen(
    _ owner: UInt64, _ root: UnsafePointer<CChar>?, _ maximumFiles: UInt64,
    _ poll: WorkspaceStreamPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_stream_search_cancelable") private func workspaceStreamSearch(
    _ owner: UInt64, _ needle: UnsafePointer<CChar>?, _ maximumMatches: UInt64,
    _ maximumBytes: UInt64, _ maximumCalls: UInt64,
    _ poll: WorkspaceStreamPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_stream_search_page") private func workspaceStreamPage(
    _ owner: UInt64, _ generation: UInt64, _ start: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_stream_search_hit") private func workspaceStreamHit(
    _ owner: UInt64, _ generation: UInt64, _ position: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_stream_search_clear") private func workspaceStreamClear(_ owner: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_cancel") private func workspaceStreamCancel(_ owner: UInt64) -> UInt8
@_silgen_name("fcb_atlas_close") private func workspaceStreamClose(_ owner: UInt64) -> UInt8
@_silgen_name("fcb_free_string") private func workspaceStreamFree(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasWorkspaceStreamTransport {
    static let native = Self(create: { workspaceStreamCreate() }, open: { owner, root, flag in
        try workspaceStreamPolling(flag) { poll, context in
            root.withCString { workspaceStreamOpen(owner, $0, AtlasWorkspaceStreamLimits.files, poll, context) }
        }
    }, search: { owner, request, flag in
        try workspaceStreamPolling(flag) { poll, context in
            request.needle.withCString {
                workspaceStreamSearch(owner, $0, request.maximumMatches, request.maximumScanBytes,
                    request.maximumReadCalls, poll, context)
            }
        }
    }, page: { try workspaceStreamTake(workspaceStreamPage($0, $1, $2, $3)) },
        hit: { try workspaceStreamTake(workspaceStreamHit($0, $1, $2)) },
        clear: { try workspaceStreamTake(workspaceStreamClear($0)) },
        cancel: { workspaceStreamCancel($0) != 0 }, close: { workspaceStreamClose($0) != 0 },
        retirementFailed: { NSLog("Large-file workspace search retirement could not be confirmed") })
}

private func workspaceStreamPolling(_ flag: AtlasSearchCancellation,
    _ work: (WorkspaceStreamPoll, UnsafeMutableRawPointer) -> UnsafeMutablePointer<CChar>?) throws -> String? {
    guard !flag.isCanceled else { throw AtlasWorkspaceStreamError.canceled }
    let poll: WorkspaceStreamPoll = { context in
        guard let context else { return 1 }
        return Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().isCanceled ? 1 : 0
    }
    return try withExtendedLifetime(flag) {
        let result = try workspaceStreamTake(work(poll, Unmanaged.passUnretained(flag).toOpaque()))
        guard !flag.isCanceled else { throw AtlasWorkspaceStreamError.canceled }
        return result
    }
}
private func workspaceStreamTake(_ pointer: UnsafeMutablePointer<CChar>?) throws -> String? {
    guard let pointer else { return nil }
    defer { workspaceStreamFree(pointer) }
    let count = strnlen(pointer, AtlasWorkspaceStreamLimits.responseBytes + 1)
    guard count <= AtlasWorkspaceStreamLimits.responseBytes,
          let text = String(data: Data(bytes: pointer, count: count), encoding: .utf8) else {
        throw AtlasWorkspaceStreamError.invalidResponse
    }
    return text
}
