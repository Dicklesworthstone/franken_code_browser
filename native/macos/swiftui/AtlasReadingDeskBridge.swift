import Foundation

@_silgen_name("fcb_desk_create") private func fcb_desk_create() -> UInt64
@_silgen_name("fcb_desk_start") private func fcb_desk_start(_ h: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_desk_state") private func fcb_desk_state(_ h: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_desk_open") private func fcb_desk_open(_ h: UInt64, _ revision: UInt64, _ attempt: UInt64, _ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_desk_apply") private func fcb_desk_apply(_ h: UInt64, _ revision: UInt64, _ attempt: UInt64, _ kind: UInt64,
    _ subject: UInt64, _ start: UInt64, _ end: UInt64, _ label: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_desk_annotate") private func fcb_desk_annotate(_ h: UInt64, _ revision: UInt64, _ attempt: UInt64,
    _ pane: UInt64, _ label: UnsafePointer<CChar>?, _ body: UnsafePointer<UInt8>?, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_desk_edit_bookmark") private func fcb_desk_edit_bookmark(_ h: UInt64, _ revision: UInt64, _ attempt: UInt64,
    _ bookmark: UInt64, _ label: UnsafePointer<CChar>?, _ body: UnsafePointer<UInt8>?, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_desk_window") private func fcb_desk_window(_ h: UInt64, _ revision: UInt64, _ pane: UInt64, _ offset: UInt64, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_desk_import_reader") private func fcb_desk_import_reader(_ h: UInt64, _ revision: UInt64, _ attempt: UInt64,
    _ reader: UInt64, _ start: UInt64, _ end: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_desk_open_reader") private func fcb_desk_open_reader(_ h: UInt64, _ revision: UInt64,
    _ pane: UInt64, _ destination: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_desk_restore") private func fcb_desk_restore(_ h: UInt64, _ revision: UInt64, _ attempt: UInt64, _ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_desk_save") private func fcb_desk_save(_ h: UInt64, _ revision: UInt64, _ path: UnsafePointer<CChar>?, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_desk_cancel") private func fcb_desk_cancel(_ h: UInt64) -> UInt8
@_silgen_name("fcb_desk_close") private func fcb_desk_close(_ h: UInt64) -> UInt8
@_silgen_name("fcb_free_string") private func deskFreeString(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasReadingDeskTransport {
    static let native = Self(create: { fcb_desk_create() }, run: { handle, request in
        func pathAllowed(_ path: String) -> Bool {
            !path.isEmpty && path.utf8.count <= 16_384 && !path.utf8.contains(0)
        }
        switch request {
        case .start: return take(fcb_desk_start(handle))
        case .state: return take(fcb_desk_state(handle))
        case .capture(let revision, let attempt, let path):
            guard pathAllowed(path) else { return nil }
            return path.withCString { take(fcb_desk_open(handle, revision, attempt, $0)) }
        case .apply(let revision, let attempt, let kind, let subject, let start, let end, let label):
            guard label.utf8.count <= 256, !label.utf8.contains(0) else { return nil }
            return label.withCString { take(fcb_desk_apply(handle, revision, attempt, kind, subject, start, end, $0)) }
        case .annotate(let revision, let attempt, let pane, let label, let body):
            guard AtlasDeskNoteTarget.validText(label: label, body: body) else { return nil }
            return label.withCString { labelPointer in
                // Counted original UTF-8 preserves embedded NUL and multiline
                // note text; only the label uses the legacy C-string contract.
                Array(body.utf8).withUnsafeBufferPointer { bytes in
                    take(fcb_desk_annotate(handle, revision, attempt, pane,
                        labelPointer, bytes.baseAddress, UInt64(bytes.count)))
                }
            }
        case .editBookmark(let revision, let attempt, let bookmark, let label, let body):
            guard AtlasDeskNoteTarget.validText(label: label, body: body) else { return nil }
            return label.withCString { labelPointer in
                Array(body.utf8).withUnsafeBufferPointer { bytes in
                    take(fcb_desk_edit_bookmark(handle, revision, attempt, bookmark,
                        labelPointer, bytes.baseAddress, UInt64(bytes.count)))
                }
            }
        case .window(let revision, let pane, let offset):
            return take(fcb_desk_window(handle, revision, pane, offset, 64 * 1024))
        case .keep(let revision, let attempt, let reader, let start, let end):
            return take(fcb_desk_import_reader(handle, revision, attempt, reader, start, end))
        case .openReader(let revision, let pane, let destination):
            return take(fcb_desk_open_reader(handle, revision, pane, destination))
        case .restore(let revision, let attempt, let path):
            guard pathAllowed(path) else { return nil }
            return path.withCString { take(fcb_desk_restore(handle, revision, attempt, $0)) }
        case .save(let revision, let path, let bytes):
            guard pathAllowed(path) else { return nil }
            return path.withCString { take(fcb_desk_save(handle, revision, $0, bytes)) }
        }
    }, cancel: { fcb_desk_cancel($0) == 1 }, close: { fcb_desk_close($0) == 1 }, retirementFailed: {
        try? FileHandle.standardError.write(contentsOf: Data("FCB_DESK_RETIREMENT_UNAVAILABLE\n".utf8))
    }, readerDomain: AtlasReaderTransport.native.comparison?.domain)

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { deskFreeString(pointer) }
        // The producer owns and terminates its allocation. Admit its bounded
        // framing before making a native copy; reject malformed UTF-8 instead
        // of replacing bytes in a response carrying source coordinates.
        var length = 0
        while length <= AtlasDeskWire.maximumResponseBytes && pointer[length] != 0 { length += 1 }
        guard length > 0, length <= AtlasDeskWire.maximumResponseBytes else { return nil }
        return String(data: Data(bytes: pointer, count: length), encoding: .utf8)
    }
}
