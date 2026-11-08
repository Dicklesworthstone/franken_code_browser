import Foundation
import Observation
import Dispatch

/// Main-actor root authority frozen before a save panel is presented. A saved
/// path alone cannot supply this authority in the App Store build.
struct AtlasSnapshotExportSourceIdentity: Equatable, Sendable {
    let root: String
    let project: UUID
    let browserOwner: UInt64
    let grant: ObjectIdentifier?
    static func == (a: Self, b: Self) -> Bool {
        a.project == b.project && a.browserOwner == b.browserOwner
            && a.grant == b.grant && a.root.utf8.elementsEqual(b.root.utf8)
    }
}
@MainActor struct AtlasSnapshotExportSource {
    let identity: AtlasSnapshotExportSourceIdentity
    let access: AnyObject?
    var root: String { identity.root }
    init(root: String, project: UUID, browserOwner: UInt64, access: AnyObject?) {
        identity = AtlasSnapshotExportSourceIdentity(root: root, project: project, browserOwner: browserOwner,
            grant: access.map(ObjectIdentifier.init))
        self.access = access
    }
}

/// Native objects are retained only for their lifetime. The worker never calls
/// framework methods through them. One admitted operation bounds the queue.
private final class AtlasSnapshotExportLease: @unchecked Sendable {
    let objects: [AnyObject]
    init(_ objects: [AnyObject]) { self.objects = objects }
}
@MainActor private final class AtlasSnapshotExportIO {
    private let queue = DispatchQueue(label: "dev.frankencode.browser.snapshot-export", qos: .userInitiated)
    func perform<Value: Sendable>(keepingAlive: [AnyObject], cancellation: AtlasSearchCancellation,
        _ work: @escaping @Sendable () throws -> Value) async throws -> Value {
        let lease = AtlasSnapshotExportLease(keepingAlive)
        return try await withCheckedThrowingContinuation { continuation in
            queue.async {
                let result = withExtendedLifetime(lease) { Result {
                    guard !cancellation.isCanceled else { throw AtlasSnapshotExportError.notStarted }
                    return try work()
                } }
                // The writer has exclusive knowledge of publication. Deliver
                // an entered result even if cancellation arrived meanwhile.
                continuation.resume(with: result)
            }
        }
    }
}

@Observable @MainActor final class AtlasSnapshotExportModel {
    private(set) var busy = false
    private(set) var cancellationRequested = false
    private(set) var request: AtlasSnapshotExportRequest?
    private(set) var receipt: AtlasSnapshotExportReceipt?
    private(set) var notice = "Save the current project as a source-containing FCBS archive for independent browsing."
    @ObservationIgnored private let transport: AtlasSnapshotExportTransport
    @ObservationIgnored private let io = AtlasSnapshotExportIO()
    @ObservationIgnored private var cancellation: AtlasSearchCancellation?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var destinationAccess: AnyObject?

    init(transport: AtlasSnapshotExportTransport) { self.transport = transport }
    var canOpenSavedArchive: Bool { !busy && receipt?.succeeded == true && request != nil }

    @discardableResult
    func save(source: AtlasSnapshotExportSource, destination: String, destinationAccess: AnyObject?,
        limits: AtlasSnapshotExportLimits = .standard) -> Bool {
        guard !busy else { return false }
        let request: AtlasSnapshotExportRequest
        do {
            guard source.identity.browserOwner > 0 else { throw AtlasSnapshotExportError.invalidInput }
            request = try AtlasSnapshotExportRequest(root: source.root, destination: destination, limits: limits)
        }
        catch {
            notice = "Choose valid absolute project and destination paths and supported capture limits. No export was started."
            return false
        }
        self.request = request; receipt = nil; self.destinationAccess = nil
        busy = true; cancellationRequested = false
        notice = "Capturing \(String(reflecting: (source.root as NSString).lastPathComponent)) into a new archive…"
        let flag = AtlasSearchCancellation(), transport = transport
        cancellation = flag
        let lifetime = [source.access, destinationAccess].compactMap { $0 }
        task = Task {
            defer {
                busy = false; cancellation = nil; task = nil
            }
            do {
                let accepted = try await io.perform(keepingAlive: lifetime, cancellation: flag) {
                    try AtlasSnapshotExportReceipt.decode(transport.save(request, flag), request: request)
                }
                receipt = accepted
                notice = accepted.message
                if accepted.succeeded { self.destinationAccess = destinationAccess }
            } catch AtlasSnapshotExportError.notStarted {
                notice = "Export canceled before the writer started. No archive was created by this request."
            } catch {
                // Null, malformed, or lost replies are not evidence of no
                // effect. Do not retry or advertise an unverified destination.
                notice = "The export outcome could not be verified. The selected destination may contain a complete or incomplete archive. Inspect it before an explicit retry, and choose a new filename."
            }
        }
        return true
    }

    func cancel() {
        guard busy, !cancellationRequested else { return }
        cancellationRequested = true; cancellation?.cancel()
        notice = "Cancellation requested. Waiting for the writer's final outcome; the selected destination may already have been created."
        // Keep the task, grants, and result channel alive until work drains.
    }

    /// An explicit user action transfers the same save-panel grant to the
    /// existing archive opener, whose descriptor then retains its own lease.
    func openSavedArchive(_ open: (String, String, AnyObject?) -> Void) {
        guard canOpenSavedArchive, let request, let digest = receipt?.summary?.digest else { return }
        open(request.destination, digest, destinationAccess)
    }
}
