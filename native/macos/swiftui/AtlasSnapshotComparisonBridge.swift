import Foundation

private typealias SnapshotComparisonPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
@_silgen_name("fcb_snapshot_comparison_create")
private func snapshotComparisonCreate() -> UInt64
@_silgen_name("fcb_snapshot_comparison_open_cancelable")
private func snapshotComparisonOpen(_ owner: UInt64, _ before: UnsafePointer<CChar>?,
    _ after: UnsafePointer<CChar>?, _ poll: SnapshotComparisonPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_snapshot_comparison_info")
private func snapshotComparisonInfo(_ owner: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_snapshot_comparison_begin_cancelable")
private func snapshotComparisonBegin(_ owner: UInt64, _ generation: UInt64,
    _ poll: SnapshotComparisonPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_snapshot_comparison_step_cancelable")
private func snapshotComparisonStep(_ owner: UInt64, _ generation: UInt64, _ maximumEntries: UInt64,
    _ poll: SnapshotComparisonPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_snapshot_comparison_page")
private func snapshotComparisonPage(_ owner: UInt64, _ generation: UInt64, _ filter: UInt64,
    _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_snapshot_comparison_open_reader")
private func snapshotComparisonReader(_ owner: UInt64, _ reader: UInt64, _ generation: UInt64,
    _ entry: UInt64, _ side: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_snapshot_comparison_cancel")
private func snapshotComparisonCancel(_ owner: UInt64) -> UInt8
@_silgen_name("fcb_snapshot_comparison_close")
private func snapshotComparisonClose(_ owner: UInt64) -> UInt8
@_silgen_name("fcb_free_string")
private func releaseSnapshotComparisonString(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasSnapshotComparisonTransport {
    static let native = Self(create: { snapshotComparisonCreate() }, open: { owner, before, after, flag in
        guard AtlasSnapshotComparisonPath.validArchive(before), AtlasSnapshotComparisonPath.validArchive(after) else {
            throw AtlasSnapshotComparisonError.invalidInput
        }
        return try before.withCString { before in try after.withCString { after in
            try polling(flag) { poll, context in try take(snapshotComparisonOpen(owner, before, after, poll, context)) }
        } }
    }, info: { try take(snapshotComparisonInfo($0)) }, begin: { owner, generation, flag in
        try polling(flag) { poll, context in try take(snapshotComparisonBegin(owner, generation, poll, context)) }
    }, step: { owner, generation, maximum, flag in
        guard (1...1024).contains(maximum) else { throw AtlasSnapshotComparisonError.invalidInput }
        return try polling(flag) { poll, context in
            try take(snapshotComparisonStep(owner, generation, maximum, poll, context))
        }
    }, page: { owner, generation, filter, start, limit in
        guard (1...128).contains(limit) else { throw AtlasSnapshotComparisonError.invalidInput }
        return try take(snapshotComparisonPage(owner, generation, filter.code, start, limit))
    }, openReader: { owner, reader, generation, entry, side in
        try take(snapshotComparisonReader(owner, reader, generation, entry, side.code))
    }, cancel: { snapshotComparisonCancel($0) == 1 }, close: { snapshotComparisonClose($0) == 1 },
        retirementFailed: {
            try? FileHandle.standardError.write(contentsOf: Data("FCB_SNAPSHOT_COMPARISON_RETIREMENT_UNAVAILABLE\n".utf8))
        }, readerDomain: AtlasReaderTransport.native.comparison?.domain)

    private static func polling<T>(_ flag: AtlasSearchCancellation,
        _ work: (SnapshotComparisonPoll, UnsafeMutableRawPointer) throws -> T) rethrows -> T {
        let poll: SnapshotComparisonPoll = { context in
            guard let context else { return 1 }
            return Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().isCanceled ? 1 : 0
        }
        return try withExtendedLifetime(flag) {
            try work(poll, Unmanaged.passUnretained(flag).toOpaque())
        }
    }
    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) throws -> String? {
        guard let pointer else { return nil }
        defer { releaseSnapshotComparisonString(pointer) }
        let count = strnlen(pointer, AtlasSnapshotComparisonWire.maximumResponseBytes + 1)
        guard count > 0, count <= AtlasSnapshotComparisonWire.maximumResponseBytes,
              let json = String(data: Data(bytes: pointer, count: count), encoding: .utf8) else {
            throw AtlasSnapshotComparisonError.invalidResponse
        }
        return json
    }
}
