import Foundation
import Observation
import Dispatch

enum AtlasReaderExportError: Error, Sendable {
    case invalidInput, sourceChanged, notStarted, unverifiedOutcome
}

/// An exact selection in one accepted capture. Presentation coordinates and
/// live filenames never enter the export writer.
struct AtlasReaderExportTarget: Sendable {
    enum Origin: Equatable, Sendable {
        case wholeCapture, page, byteRange
        case textSelection(UUID), comparison(UUID)
    }
    let identity: AtlasReaderIdentity
    let captureSHA256: String
    let navigation: UUID
    let origin: Origin
    let start, end: UInt64
    var byteCount: UInt64 { end - start }

    init(identity: AtlasReaderIdentity, captureSHA256: String, navigation: UUID,
         origin: Origin, start: UInt64, end: UInt64) throws {
        guard identity.owner > 0, identity.file > 0, identity.revision > 0,
              identity.capturedBytes <= AtlasReaderLimits.captureBytes,
              Self.validDigest(captureSHA256), start <= end, end <= identity.capturedBytes else {
            throw AtlasReaderExportError.invalidInput
        }
        self.identity = identity; self.captureSHA256 = captureSHA256; self.navigation = navigation
        self.origin = origin; self.start = start; self.end = end
    }
    static func validDigest(_ value: String) -> Bool {
        value.utf8.count == 64 && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
}

struct AtlasReaderExportRequest: Sendable {
    let target: AtlasReaderExportTarget
    let destination: String
    init(target: AtlasReaderExportTarget, destination: String) throws {
        let bytes = destination.utf8
        guard bytes.first == 47, bytes.count > 1, bytes.count <= 16_384, !bytes.contains(0),
              !bytes.dropFirst().split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                  $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
              }) else { throw AtlasReaderExportError.invalidInput }
        self.target = target; self.destination = destination
    }
}

struct AtlasReaderExportTransport: Sendable {
    /// A foreign reader handle is meaningful only in this loaded registry.
    /// Nil disables export rather than sending a handle to another registry.
    let domain: UUID?
    let save: @Sendable (UInt64, AtlasReaderExportRequest, AtlasSearchCancellation) throws -> String?
}

enum AtlasReaderExportEffect: String, Decodable, Sendable {
    case none
    case incomplete = "destination-created-incomplete"
    case written = "complete-file-sync-unconfirmed"
    case synced = "complete-file-sync-requested"
    case parentSynced = "complete-file-and-parent-sync-requested"
}

/// Once the writer is entered, cancellation may coexist with a created file.
/// This receipt is delivered independently of the reader's later UI identity.
struct AtlasReaderExportReceipt: Sendable {
    static let maximumResponseBytes = 256 * 1024
    let effect: AtlasReaderExportEffect
    let bytesWritten: UInt64
    let complete: Bool
    let errorCode: String?
    var succeeded: Bool { complete && errorCode == nil && (effect == .synced || effect == .parentSynced) }
    var message: String {
        if succeeded { return "Exported \(bytesWritten) original captured bytes to the selected file." }
        let code = errorCode ?? "unverified"
        switch effect {
        case .none:
            return "No export file was created (\(code)). Existing destinations are never overwritten. Choose a new filename to try again."
        case .incomplete:
            return "The destination contains an incomplete export: \(bytesWritten) bytes were written (\(code)). Inspect it before trying again; choose a new filename."
        case .written:
            return "All \(bytesWritten) original bytes were written, but file synchronization was not confirmed (\(code)). Inspect the destination before trying again."
        case .synced, .parentSynced:
            return "All \(bytesWritten) original bytes were written and synchronization was requested, but the operation reported \(code). Inspect the destination before trying again."
        }
    }

    static func decode(_ json: String?, request: AtlasReaderExportRequest) throws -> Self {
        guard let json, !json.isEmpty, json.utf8.count <= maximumResponseBytes else {
            throw AtlasReaderExportError.unverifiedOutcome
        }
        do {
            let data = Data(json.utf8)
            let header = try JSONDecoder().decode(Header.self, from: data)
            if header.schema == "fcb.reader-session/1" {
                // This distinct schema is used ONLY before the export writer
                // starts. Refuse invented effect fields or mixed success data.
                let failure = try JSONDecoder().decode(AdmissionFailure.self, from: data)
                guard failure.status == "error", failure.owner.value == request.target.identity.owner,
                      failure.exit_code.value == 2 || failure.exit_code.value == 130,
                      validCode(failure.error.code),
                      let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                      Set(object.keys) == Set(["schema", "status", "owner", "exit_code", "error"]) else {
                    throw AtlasReaderExportError.unverifiedOutcome
                }
                return Self(effect: .none, bytesWritten: 0, complete: false, errorCode: failure.error.code)
            }
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let target = request.target, identity = target.identity
            guard wire.schema == "fcb.reader-export/1", wire.command == "export-original",
                  wire.owner.value == identity.owner, wire.file_id.value == identity.file,
                  wire.source_revision.value == identity.revision,
                  wire.captured_bytes.value == identity.capturedBytes,
                  wire.capture_sha256 == target.captureSHA256,
                  wire.original_range.start.value == target.start, wire.original_range.end.value == target.end,
                  wire.export_bytes.value == target.byteCount, wire.bytes_written.value <= target.byteCount,
                  wire.destination.bytes == Array(request.destination.utf8),
                  wire.export_domain == "original-bytes", wire.additional_source_bytes_read.value == 0,
                  !wire.overwritten, !wire.power_loss_qualified else {
                throw AtlasReaderExportError.unverifiedOutcome
            }
            switch wire.effect {
            case .none:
                guard !wire.complete, wire.bytes_written.value == 0 else { throw AtlasReaderExportError.unverifiedOutcome }
            case .incomplete:
                guard !wire.complete else { throw AtlasReaderExportError.unverifiedOutcome }
            case .written, .synced, .parentSynced:
                guard wire.complete, wire.bytes_written.value == target.byteCount else {
                    throw AtlasReaderExportError.unverifiedOutcome
                }
            }
            if wire.status == "ok" {
                guard wire.error == nil, wire.complete, wire.effect == .synced || wire.effect == .parentSynced else {
                    throw AtlasReaderExportError.unverifiedOutcome
                }
            } else {
                guard wire.status == "error", let error = wire.error, validCode(error.code),
                      error.code.hasPrefix("READER_EXPORT_"),
                      !error.next_action.isEmpty, error.next_action.utf8.count <= 4096 else {
                    throw AtlasReaderExportError.unverifiedOutcome
                }
            }
            return Self(effect: wire.effect, bytesWritten: wire.bytes_written.value,
                complete: wire.complete, errorCode: wire.error?.code)
        } catch { throw AtlasReaderExportError.unverifiedOutcome }
    }

    private static func validCode(_ code: String) -> Bool {
        !code.isEmpty && code.utf8.count <= 512 && code.utf8.allSatisfy { (33...126).contains($0) }
    }
    private struct Header: Decodable { let schema: String }
    private struct Number: Decodable {
        let value: UInt64
        init(from decoder: Decoder) throws {
            let text = try decoder.singleValueContainer().decode(String.self)
            guard let value = UInt64(text), String(value) == text else { throw AtlasReaderExportError.unverifiedOutcome }
            self.value = value
        }
    }
    private struct Range: Decodable { let start, end: Number }
    private struct Path: Decodable {
        let bytes: [UInt8]
        private enum CodingKeys: CodingKey { case encoding, hex, display }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let hex = try c.decode(String.self, forKey: .hex)
            guard try c.decode(String.self, forKey: .encoding) == "unix-bytes",
                  try c.decode(String.self, forKey: .display).utf8.count <= 65_536,
                  !hex.isEmpty, hex.utf8.count <= 32_768, hex.utf8.count.isMultiple(of: 2) else {
                throw AtlasReaderExportError.unverifiedOutcome
            }
            let digits = Array(hex.utf8)
            func nibble(_ byte: UInt8) throws -> UInt8 {
                switch byte {
                case 48...57: return byte - 48
                case 97...102: return byte - 87
                default: throw AtlasReaderExportError.unverifiedOutcome
                }
            }
            var bytes: [UInt8] = []; bytes.reserveCapacity(digits.count / 2)
            for at in stride(from: 0, to: digits.count, by: 2) {
                bytes.append(try (nibble(digits[at]) << 4) | nibble(digits[at + 1]))
            }
            guard bytes.first == 47, !bytes.contains(0) else { throw AtlasReaderExportError.unverifiedOutcome }
            self.bytes = bytes
        }
    }
    private struct Failure: Decodable { let code, next_action: String }
    private struct AdmissionFailure: Decodable {
        struct Detail: Decodable { let code: String }
        let status: String
        let owner, exit_code: Number
        let error: Detail
    }
    private struct Wire: Decodable {
        let schema, command, status: String
        let owner, file_id, source_revision, captured_bytes, export_bytes, bytes_written: Number
        let capture_sha256, export_domain: String
        let original_range: Range
        let destination: Path
        let additional_source_bytes_read: Number
        let effect: AtlasReaderExportEffect
        let complete, overwritten, power_loss_qualified: Bool
        let error: Failure?
    }
}

/// Native objects are retained for lifetime only. One accepted operation per
/// model admits at most one worker payload; no source bytes are copied here.
private final class AtlasReaderExportLease: @unchecked Sendable {
    let objects: [AnyObject]
    init(_ objects: [AnyObject]) { self.objects = objects }
}
@MainActor private final class AtlasReaderExportIO {
    private let queue = DispatchQueue(label: "dev.frankencode.browser.reader-export", qos: .userInitiated)
    func perform(keepingAlive: [AnyObject], cancellation: AtlasSearchCancellation,
        _ work: @escaping @Sendable () throws -> AtlasReaderExportReceipt) async throws -> AtlasReaderExportReceipt {
        let lease = AtlasReaderExportLease(keepingAlive)
        return try await withCheckedThrowingContinuation { continuation in
            queue.async {
                let result = withExtendedLifetime(lease) { Result {
                    guard !cancellation.isCanceled else { throw AtlasReaderExportError.notStarted }
                    return try work()
                } }
                // Never filter an entered writer's receipt after cancellation.
                continuation.resume(with: result)
            }
        }
    }
}

@Observable @MainActor final class AtlasReaderExportModel {
    private(set) var busy = false
    private(set) var cancellationRequested = false
    private(set) var request: AtlasReaderExportRequest?
    private(set) var receipt: AtlasReaderExportReceipt?
    private(set) var notice = ""
    @ObservationIgnored private let transport: AtlasReaderExportTransport
    @ObservationIgnored private let io = AtlasReaderExportIO()
    @ObservationIgnored private var cancellation: AtlasSearchCancellation?
    @ObservationIgnored private var task: Task<Void, Never>?

    init(transport: AtlasReaderExportTransport) { self.transport = transport }
    var domain: UUID? { transport.domain }

    @discardableResult
    func save(source: AtlasReaderRetainedExportSource, target: AtlasReaderExportTarget,
              destination: String, destinationAccess: AnyObject?) -> Bool {
        guard !busy else { return false }
        let request: AtlasReaderExportRequest
        do {
            guard let domain, source.registryDomain == domain,
                  source.handle == target.identity.owner, source.identity.matches(target.identity),
                  source.captureSHA256 == target.captureSHA256 else { throw AtlasReaderExportError.sourceChanged }
            request = try AtlasReaderExportRequest(target: target, destination: destination)
        } catch {
            notice = "The retained source or destination changed. Review the current capture and choose Export again. No export was started."
            return false
        }
        self.request = request; receipt = nil; busy = true; cancellationRequested = false
        notice = "Exporting \(target.byteCount) original captured bytes…"
        let flag = AtlasSearchCancellation(), transport = transport, handle = source.handle
        cancellation = flag
        let lifetime = [source.lifetime, destinationAccess].compactMap { $0 }
        task = Task {
            defer { busy = false; cancellation = nil; task = nil }
            do {
                let accepted = try await io.perform(keepingAlive: lifetime, cancellation: flag) {
                    try AtlasReaderExportReceipt.decode(transport.save(handle, request, flag), request: request)
                }
                receipt = accepted
                notice = accepted.message
            } catch AtlasReaderExportError.notStarted {
                notice = "Export canceled before the writer started. No export file was created by this request."
            } catch {
                notice = "The export outcome could not be verified. The selected destination may contain complete or incomplete bytes. Inspect it before an explicit retry, and choose a new filename."
            }
        }
        return true
    }

    func cancel() {
        guard busy, !cancellationRequested else { return }
        cancellationRequested = true; cancellation?.cancel()
        notice = "Cancellation requested. Waiting for the writer's final outcome; the destination may already have been created."
        // Do not cancel the Swift task or release grants before the writer drains.
    }
}

@MainActor extension AtlasPagedReaderModel {
    func originalExportTarget(_ origin: AtlasReaderExportTarget.Origin,
                              start: UInt64? = nil, end: UInt64? = nil) -> AtlasReaderExportTarget? {
        guard !busy, !pinning, !documentPreview.busy, !resolvingTextSelection,
              let page, let digest = retainedCaptureSHA256 else { return nil }
        let first, last: UInt64
        switch origin {
        case .wholeCapture: first = 0; last = page.identity.capturedBytes
        case .page: first = page.start; last = page.end
        case .byteRange:
            guard let start, let end else { return nil }
            first = start; last = end
        case .textSelection(let id):
            guard let selection = textSelection, selection.id == id, selection.identity.matches(page.identity) else { return nil }
            first = selection.originalStart; last = selection.originalEnd
        case .comparison(let id):
            guard let selection = currentComparisonSelection, selection.id == id else { return nil }
            first = selection.range.start; last = selection.range.end
        }
        return try? AtlasReaderExportTarget(identity: page.identity, captureSHA256: digest, navigation: navigation,
            origin: origin, start: first, end: last)
    }

    func acceptsOriginalExport(_ target: AtlasReaderExportTarget) -> Bool {
        guard let current = originalExportTarget(target.origin, start: target.start, end: target.end) else { return false }
        return current.identity.matches(target.identity) && current.captureSHA256 == target.captureSHA256
            && current.navigation == target.navigation && current.start == target.start && current.end == target.end
    }
}
