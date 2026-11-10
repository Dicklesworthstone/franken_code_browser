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
@_silgen_name("fcb_observed_reader_lines") private func observedReaderLines(
    _ handle: UInt64, _ firstLine: UInt64, _ lines: UInt64, _ maximumBytes: UInt64,
    _ maximumScanBytes: UInt64, _ encoding: UInt64,
    _ poll: ObservedReaderPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_observed_reader_copy") private func observedReaderCopy(
    _ handle: UInt64, _ observation: UInt64, _ start: UInt64, _ end: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_observed_reader_find") private func observedReaderFind(
    _ handle: UInt64, _ generation: UInt64, _ needle: UnsafePointer<CChar>?, _ encoding: UInt64,
    _ maximumMatches: UInt64, _ maximumScanBytes: UInt64,
    _ poll: ObservedReaderPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_observed_reader_search_hit") private func observedReaderSearchHit(
    _ handle: UInt64, _ generation: UInt64, _ index: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_observed_reader_observe_hit") private func observedReaderObserveHit(
    _ handle: UInt64, _ generation: UInt64, _ index: UInt64, _ contextBytes: UInt64, _ windowBytes: UInt64,
    _ poll: ObservedReaderPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
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
        retirementFailed: { NSLog("Observed range reader retirement could not be confirmed") },
        search: .init(find: { handle, request, cancellation in
            try observedReaderPolling(cancellation) { poll, context in
                request.needle.withCString { needle in
                    observedReaderFind(handle, request.generation, needle, request.encoding.abi,
                        request.maximumMatches, request.maximumScanBytes, poll, context)
                }
            }
        }, hit: { try observedReaderTake(observedReaderSearchHit($0, $1, $2)) },
        observeHit: { handle, witness, request, cancellation in
            let target = witness.target, range = witness.target.hit.range
            guard target.identity.owner == handle, target.generation > 0,
                  target.hit.index < AtlasObservedReaderSearchLimits.matches,
                  request.encoding == witness.encoding, request.offset <= range.start,
                  range.count > 0, request.bytes >= 8, range.count <= request.bytes - 8 else {
                throw AtlasObservedReaderError.invalidInput
            }
            let prefix = range.start - request.offset
            // The host reserves eight bytes for scalar/CRLF boundary floors.
            // Refuse a request whose desired prefix would be shortened there.
            guard prefix <= request.bytes - range.count - 8 else { throw AtlasObservedReaderError.invalidInput }
            return try observedReaderPolling(cancellation) { poll, context in
                observedReaderObserveHit(handle, target.generation, target.hit.index, prefix, request.bytes, poll, context)
            }
        }), lines: { handle, request, cancellation in
            try observedReaderPolling(cancellation) { poll, context in
                observedReaderLines(handle, request.firstLine, request.lines, request.maximumSelectedBytes,
                    request.maximumScanBytes, request.encoding.abi, poll, context)
            }
        })
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
