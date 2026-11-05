import Foundation
import Observation

/// A native folder choice or previously stored OS bookmark, never a path that
/// pretends to grant access. The resolver performs all native filesystem work.
enum AtlasRootAccessRequest: Sendable {
    case selected(URL)
    case restored(Data)

    static let maximumBookmarkBytes = 1024 * 1024
    var isValid: Bool {
        switch self {
        case .selected(let url):
            return url.isFileURL && (url.host == nil || url.host == "" || url.host == "localhost")
                && url.path.hasPrefix("/") && url.path.utf8.count <= 16_384 && !url.path.utf8.contains(0)
        case .restored(let bookmark):
            return !bookmark.isEmpty && bookmark.count <= Self.maximumBookmarkBytes
        }
    }
}

enum AtlasRootAccessError: Error, Sendable {
    case invalidRequest, unavailable
}

/// Security-scoped bookmark resolution may block in Foundation. Reuse the
/// bounded native-host I/O lane: one active call and one newest pending request.
/// Cancellation invalidates delivery immediately and drains the foreign call
/// with its input lease intact. Only the current result may activate a project
/// or persist a refreshed bookmark on the main actor.
@Observable @MainActor final class AtlasRootAccessLoader<Access: Sendable> {
    private(set) var isLoading = false
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private let resolve: @Sendable (AtlasRootAccessRequest, AtlasSearchCancellation) throws -> Access
    @ObservationIgnored private var generation = UUID()
    @ObservationIgnored private var cancellation: AtlasSearchCancellation?
    @ObservationIgnored private var task: Task<Void, Never>?

    init(resolve: @escaping @Sendable (AtlasRootAccessRequest, AtlasSearchCancellation) throws -> Access) {
        self.resolve = resolve
    }

    func accepts(_ ticket: UUID) -> Bool { ticket == generation }

    func open(_ request: AtlasRootAccessRequest, ifCurrent ticket: UUID? = nil, keepingAlive leases: [AnyObject] = [],
        completion: @escaping @MainActor (Result<Access, AtlasRootAccessError>) -> Void) {
        if let ticket, !accepts(ticket) { return }
        cancel()
        guard request.isValid, leases.count <= 4 else { completion(.failure(.invalidRequest)); return }
        let ticket = generation, flag = AtlasSearchCancellation(), resolve = resolve
        cancellation = flag; isLoading = true
        task = Task { [self] in
            let result: Result<Access, AtlasRootAccessError>
            do {
                let access = try await io.perform(cancellation: flag, keepingAlive: leases) {
                    try resolve(request, flag)
                }
                result = .success(access)
            } catch {
                result = .failure((error as? AtlasRootAccessError) ?? .unavailable)
            }
            guard ticket == generation, !flag.isCanceled, !Task.isCancelled else { return }
            isLoading = false; cancellation = nil; task = nil
            completion(result)
        }
    }

    /// Call on window disappearance and before replacing an in-flight choice.
    /// No wait and no early release of a grant used by an entered resolver.
    @discardableResult func cancel() -> UUID {
        generation = UUID()
        cancellation?.cancel(); cancellation = nil
        task?.cancel(); task = nil
        io.cancel(); isLoading = false
        return generation
    }
}
