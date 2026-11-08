import Foundation

private typealias ObservedReaderPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
@_silgen_name("fcb_observed_reader_create") private func observedReaderCreate() -> UInt64
@_silgen_name("fcb_observed_reader_open") private func observedReaderOpen(
    _ handle: UInt64, _ root: UnsafePointer<CChar>?, _ relativeHex: UnsafePointer<CChar>?,
    _ poll: ObservedReaderPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_observed_reader_info") private func observedReaderInfo(_ handle: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_observed_reader_window") private func observedReaderWindow(
    _ handle: UInt64, _ offset: UInt64, _ bytes: UInt64, _ encoding: UInt64,
    _ poll: ObservedReaderPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_observed_reader_retained") private func observedReaderRetained(
    _ handle: UInt64, _ observation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_observed_reader_copy") private func observedReaderCopy(
    _ handle: UInt64, _ observation: UInt64, _ start: UInt64, _ end: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_observed_reader_cancel") private func observedReaderCancel(_ handle: UInt64) -> UInt8
@_silgen_name("fcb_observed_reader_close") private func observedReaderClose(_ handle: UInt64) -> UInt8
@_silgen_name("fcb_free_string") private func observedReaderFree(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasObservedReaderTransport {
    static let native = Self(
        create: { observedReaderCreate() },
        open: { handle, input, cancellation in
            try observedReaderPolling(cancellation) { poll, context in
                input.root.withCString { root in
                    input.relativeHex.withCString { relative in
                        observedReaderOpen(handle, root, relative, poll, context)
                    }
                }
            }
        },
        info: { try observedReaderTake(observedReaderInfo($0)) },
        window: { handle, request, cancellation in
            try observedReaderPolling(cancellation) { poll, context in
                observedReaderWindow(handle, request.offset, request.bytes, request.encoding.abi, poll, context)
            }
        },
        retained: { try observedReaderTake(observedReaderRetained($0, $1)) },
        copy: { try observedReaderTake(observedReaderCopy($0, $1, $2, $3)) },
        cancel: { observedReaderCancel($0) != 0 }, close: { observedReaderClose($0) != 0 },
        retirementFailed: { NSLog("Observed range reader retirement could not be confirmed") })
}

private func observedReaderPolling(_ cancellation: AtlasSearchCancellation,
    _ call: (ObservedReaderPoll, UnsafeMutableRawPointer) -> UnsafeMutablePointer<CChar>?) throws -> String? {
    guard !cancellation.isCanceled else { throw AtlasObservedReaderError.canceled }
    let poll: ObservedReaderPoll = { context in
        guard let context else { return 1 }
        return Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().isCanceled ? 1 : 0
    }
    return try withExtendedLifetime(cancellation) {
        let result = try observedReaderTake(call(poll, Unmanaged.passUnretained(cancellation).toOpaque()))
        // Read-only observations carry no external write effect. Obsolete
        // replies are freed and suppressed without substituting new live bytes.
        guard !cancellation.isCanceled else { throw AtlasObservedReaderError.canceled }
        return result
    }
}

private func observedReaderTake(_ pointer: UnsafeMutablePointer<CChar>?) throws -> String? {
    guard let pointer else { return nil }
    defer { observedReaderFree(pointer) }
    let count = strnlen(pointer, AtlasObservedReaderLimits.responseBytes + 1)
    guard count <= AtlasObservedReaderLimits.responseBytes,
          let text = String(data: Data(bytes: pointer, count: count), encoding: .utf8) else {
        throw AtlasObservedReaderError.invalidResponse
    }
    return text
}
