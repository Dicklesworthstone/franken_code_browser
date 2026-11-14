import Foundation

private typealias WorkspaceStreamPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
private typealias WorkspaceStreamProgress = @convention(c) (
    UnsafePointer<UInt8>?, UInt64, UnsafeMutableRawPointer?) -> Void
@_silgen_name("fcb_atlas_create") private func workspaceStreamCreate() -> UInt64
@_silgen_name("fcb_atlas_open_cancelable") private func workspaceStreamOpen(
    _ owner: UInt64, _ root: UnsafePointer<CChar>?, _ maximumFiles: UInt64,
    _ poll: WorkspaceStreamPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_stream_search_scoped_cancelable") private func workspaceStreamSearch(
    _ owner: UInt64, _ needle: UnsafePointer<CChar>?, _ maximumMatches: UInt64,
    _ maximumBytes: UInt64, _ maximumCalls: UInt64,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?,
    _ poll: WorkspaceStreamPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_stream_search_scoped_progress") private func workspaceStreamSearchProgress(
    _ owner: UInt64, _ needle: UnsafePointer<CChar>?, _ maximumMatches: UInt64,
    _ maximumBytes: UInt64, _ maximumCalls: UInt64,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?, _ poll: WorkspaceStreamPoll?,
    _ stop: WorkspaceStreamPoll?, _ progress: WorkspaceStreamProgress?,
    _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_stream_search_scoped_counted_progress") private func workspaceStreamSearchCountedProgress(
    _ owner: UInt64, _ needle: UnsafePointer<CChar>?, _ maximumMatches: UInt64,
    _ maximumBytes: UInt64, _ maximumCalls: UInt64,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?, _ poll: WorkspaceStreamPoll?,
    _ stop: WorkspaceStreamPoll?, _ progress: WorkspaceStreamProgress?,
    _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_stream_search_scoped_matching_progress") private func workspaceStreamSearchMatchingProgress(
    _ owner: UInt64, _ needle: UnsafePointer<CChar>?, _ maximumMatches: UInt64,
    _ maximumBytes: UInt64, _ maximumCalls: UInt64,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?, _ caseSensitive: UInt64, _ countAllMatches: UInt64,
    _ poll: WorkspaceStreamPoll?, _ stop: WorkspaceStreamPoll?, _ progress: WorkspaceStreamProgress?,
    _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_stream_search_scoped_expression_progress") private func workspaceStreamExpressionProgress(
    _ owner: UInt64, _ expression: UnsafePointer<CChar>?, _ maximumMatches: UInt64,
    _ maximumBytes: UInt64, _ maximumCalls: UInt64,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?, _ caseSensitive: UInt64,
    _ poll: WorkspaceStreamPoll?, _ stop: WorkspaceStreamPoll?, _ progress: WorkspaceStreamProgress?,
    _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_stream_search_scoped_occurrences_progress") private func workspaceStreamOccurrencesProgress(
    _ owner: UInt64, _ needle: UnsafePointer<CChar>?, _ maximumMatches: UInt64,
    _ maximumBytes: UInt64, _ maximumCalls: UInt64,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?, _ countAllMatches: UInt64,
    _ poll: WorkspaceStreamPoll?, _ stop: WorkspaceStreamPoll?, _ progress: WorkspaceStreamProgress?,
    _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_stream_search_scoped_page_progress") private func workspaceStreamSearchPageProgress(
    _ owner: UInt64, _ needle: UnsafePointer<CChar>?, _ maximumMatches: UInt64,
    _ maximumBytes: UInt64, _ maximumCalls: UInt64,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?, _ caseSensitive: UInt64,
    _ countAllMatches: UInt64, _ occurrences: UInt64, _ startAfter: UInt64,
    _ poll: WorkspaceStreamPoll?, _ stop: WorkspaceStreamPoll?, _ progress: WorkspaceStreamProgress?,
    _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
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
            request.needle.withCString { needle in
                request.fileScope.withCStrings { extensions, filenames in
                    request.pathScope.withCString { kind, path in
                        if request.pageStartAfter > 0 {
                            return workspaceStreamSearchPageProgress(owner, needle, request.maximumMatches,
                                request.maximumScanBytes, request.maximumReadCalls, extensions, filenames,
                                kind, path, request.caseSensitive ? 1 : 0, request.countAllMatches ? 1 : 0,
                                request.mode == .occurrences ? 1 : 0, request.pageStartAfter, poll, nil, nil, context)
                        }
                        if request.mode == .expression {
                            return workspaceStreamExpressionProgress(owner, needle, request.maximumMatches,
                                request.maximumScanBytes, request.maximumReadCalls, extensions, filenames,
                                kind, path, request.caseSensitive ? 1 : 0, poll, nil, nil, context)
                        }
                        if request.mode == .occurrences {
                            return workspaceStreamOccurrencesProgress(owner, needle, request.maximumMatches,
                                request.maximumScanBytes, request.maximumReadCalls, extensions, filenames,
                                kind, path, request.countAllMatches ? 1 : 0, poll, nil, nil, context)
                        }
                        if !request.caseSensitive {
                            return workspaceStreamSearchMatchingProgress(owner, needle, request.maximumMatches,
                                request.maximumScanBytes, request.maximumReadCalls, extensions, filenames,
                                kind, path, 0, request.countAllMatches ? 1 : 0, poll, nil, nil, context)
                        }
                        if request.countAllMatches {
                            return workspaceStreamSearchCountedProgress(owner, needle, request.maximumMatches,
                                request.maximumScanBytes, request.maximumReadCalls, extensions, filenames,
                                kind, path, poll, nil, nil, context)
                        }
                        return workspaceStreamSearch(owner, needle, request.maximumMatches, request.maximumScanBytes,
                            request.maximumReadCalls, extensions, filenames, kind, path, poll, context)
                    }
                }
            }
        }
    }, page: { try workspaceStreamTake(workspaceStreamPage($0, $1, $2, $3)) },
        hit: { try workspaceStreamTake(workspaceStreamHit($0, $1, $2)) },
        clear: { try workspaceStreamTake(workspaceStreamClear($0)) },
        cancel: { workspaceStreamCancel($0) != 0 }, close: { workspaceStreamClose($0) != 0 },
        retirementFailed: { NSLog("Large-file workspace search retirement could not be confirmed") },
        progressiveSearch: { owner, request, flag, stop, progress in
            try workspaceStreamProgressive(owner, request, flag, stop, progress)
        })
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


// This box is owned exclusively by the entered synchronous worker. The C ABI
// does not retain it; only its two independently synchronized flags cross
// threads. A callback copies its borrowed byte range before notifying Swift.
private final class WorkspaceStreamProgressContext {
    let cancellation: AtlasSearchCancellation
    let stop: AtlasWorkspaceStreamStop
    let progress: @Sendable (String) -> Void
    var invalid = false

    init(_ cancellation: AtlasSearchCancellation, _ stop: AtlasWorkspaceStreamStop,
         _ progress: @escaping @Sendable (String) -> Void) {
        self.cancellation = cancellation; self.stop = stop; self.progress = progress
    }
}

private func workspaceStreamProgressive(_ owner: UInt64, _ request: AtlasWorkspaceStreamRequest,
    _ flag: AtlasSearchCancellation, _ stop: AtlasWorkspaceStreamStop,
    _ progress: @Sendable (String) -> Void) throws -> String? {
    guard !flag.isCanceled else { throw AtlasWorkspaceStreamError.canceled }
    // The sink's apparent escape into the context is bounded by the entered
    // synchronous C call. Neither the box nor callback survives this scope.
    return try withoutActuallyEscaping(progress) { sink in
        let box = WorkspaceStreamProgressContext(flag, stop, sink)
        let poll: WorkspaceStreamPoll = { context in
            guard let context else { return 1 }
            let box = Unmanaged<WorkspaceStreamProgressContext>.fromOpaque(context).takeUnretainedValue()
            return box.invalid || box.cancellation.isCanceled ? 1 : 0
        }
        let stopped: WorkspaceStreamPoll = { context in
            guard let context else { return 0 }
            return Unmanaged<WorkspaceStreamProgressContext>.fromOpaque(context)
                .takeUnretainedValue().stop.isRequested ? 1 : 0
        }
        let receive: WorkspaceStreamProgress = { bytes, length, context in
            guard let context else { return }
            let box = Unmanaged<WorkspaceStreamProgressContext>.fromOpaque(context).takeUnretainedValue()
            guard !box.invalid, !box.cancellation.isCanceled else { return }
            guard let bytes, length > 0,
                  length <= UInt64(AtlasWorkspaceStreamLimits.responseBytes),
                  let copied = String(data: Data(bytes: bytes, count: Int(length)), encoding: .utf8) else {
                box.invalid = true; return
            }
            box.progress(copied)
        }
        return try withExtendedLifetime(box) {
            let pointer = request.needle.withCString { needle in
                request.fileScope.withCStrings { extensions, filenames in
                    request.pathScope.withCString { kind, path in
                        if request.pageStartAfter > 0 {
                            return workspaceStreamSearchPageProgress(owner, needle, request.maximumMatches,
                                request.maximumScanBytes, request.maximumReadCalls, extensions, filenames,
                                kind, path, request.caseSensitive ? 1 : 0, request.countAllMatches ? 1 : 0,
                                request.mode == .occurrences ? 1 : 0, request.pageStartAfter,
                                poll, stopped, receive, Unmanaged.passUnretained(box).toOpaque())
                        }
                        if request.mode == .expression {
                            return workspaceStreamExpressionProgress(owner, needle, request.maximumMatches,
                                request.maximumScanBytes, request.maximumReadCalls, extensions, filenames,
                                kind, path, request.caseSensitive ? 1 : 0,
                                poll, stopped, receive, Unmanaged.passUnretained(box).toOpaque())
                        }
                        if request.mode == .occurrences {
                            return workspaceStreamOccurrencesProgress(owner, needle, request.maximumMatches,
                                request.maximumScanBytes, request.maximumReadCalls, extensions, filenames,
                                kind, path, request.countAllMatches ? 1 : 0,
                                poll, stopped, receive, Unmanaged.passUnretained(box).toOpaque())
                        }
                        if !request.caseSensitive {
                            return workspaceStreamSearchMatchingProgress(owner, needle, request.maximumMatches,
                                request.maximumScanBytes, request.maximumReadCalls, extensions, filenames,
                                kind, path, 0, request.countAllMatches ? 1 : 0,
                                poll, stopped, receive, Unmanaged.passUnretained(box).toOpaque())
                        }
                        if request.countAllMatches {
                            return workspaceStreamSearchCountedProgress(owner, needle, request.maximumMatches,
                                request.maximumScanBytes, request.maximumReadCalls, extensions, filenames,
                                kind, path, poll, stopped, receive, Unmanaged.passUnretained(box).toOpaque())
                        }
                        return workspaceStreamSearchProgress(owner, needle, request.maximumMatches,
                            request.maximumScanBytes, request.maximumReadCalls, extensions, filenames,
                            kind, path, poll, stopped, receive, Unmanaged.passUnretained(box).toOpaque())
                    }
                }
            }
            // Even malformed progress or late cancellation must release the
            // independently owned terminal reply exactly once.
            let result = Result { try workspaceStreamTake(pointer) }
            if box.invalid { throw AtlasWorkspaceStreamError.invalidResponse }
            guard !flag.isCanceled else { throw AtlasWorkspaceStreamError.canceled }
            return try result.get()
        }
    }
}
