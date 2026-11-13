import Foundation
import Observation

/// Native ownership of a short Markdown link trail. Every destination is an
/// already verified, independently retained capture; moving through this
/// history neither opens a pathname nor replays an old catalog/query receipt.
@Observable @MainActor final class AtlasDocumentLinkHistory<Capture: AnyObject> {
    /// Leave room in the eight-reader registry for the originating source,
    /// two pins, and one candidate. Registry admission still applies when other
    /// independent readers are open; a refusal never discards accepted history.
    static var maximumCaptures: Int { 3 }
    private(set) var captures: [Capture] = []
    private(set) var position = 0
    private(set) var busy = false
    private(set) var canceling = false
    private(set) var bound = false
    private(set) var notice = ""

    @ObservationIgnored private var scope: Scope?
    @ObservationIgnored private var epoch = UUID()
    @ObservationIgnored private var operation: UUID?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private let retire: @MainActor (Capture) -> Void
    @ObservationIgnored private let failureMessage: @MainActor (Error) -> String

    private struct Scope: Equatable {
        let root: [UInt8]
        let access: ObjectIdentifier?
        init?(root: String, access: ObjectIdentifier?) {
            guard root.hasPrefix("/"), root.utf8.count <= 16_384,
                  !root.utf8.contains(0) else { return nil }
            self.root = Array(root.utf8)
            self.access = access
        }
    }

    init(retire: @escaping @MainActor (Capture) -> Void,
         failureMessage: @escaping @MainActor (Error) -> String) {
        self.retire = retire
        self.failureMessage = failureMessage
    }
    deinit { task?.cancel() }

    var current: Capture? { captures.indices.contains(position) ? captures[position] : nil }
    var canFollow: Bool { bound && !busy }
    var canBack: Bool { !busy && position > 0 && current != nil }
    var canForward: Bool { !busy && position + 1 < captures.count }

    /// A successful same-root catalog refresh preserves captured history.
    /// Replacing the native grant, even for identical path bytes, revokes it.
    /// The return value tells the host whether the existing scope survived.
    @discardableResult
    func bind(root: String, accessIdentity: ObjectIdentifier?) -> Bool {
        guard let next = Scope(root: root, access: accessIdentity) else {
            close()
            return false
        }
        guard next != scope else { return true }
        close()
        scope = next
        bound = true
        return false
    }

    /// The caller supplies the existing verified project-link operation.
    /// Keep its single admission slot occupied until the foreign call drains,
    /// including cancellation, root replacement, and sheet dismissal.
    func follow(_ open: @escaping @MainActor () async throws -> Capture) {
        guard canFollow, let scope else { return }
        let ticket = UUID(), acceptedEpoch = epoch
        let retire = retire, failureMessage = failureMessage
        operation = ticket
        busy = true
        canceling = false
        notice = "Opening the linked source…"
        task = Task { [weak self] in
            do {
                try Task.checkCancellation()
                let candidate = try await open()
                guard let self else { retire(candidate); return }
                defer { self.finish(ticket) }
                guard self.operation == ticket, self.epoch == acceptedEpoch,
                      self.scope == scope, !self.canceling, !Task.isCancelled else {
                    retire(candidate)
                    return
                }
                // Verified project opens create independent reader owners.
                // Do not retire an already owned entry if a host mistakenly
                // returns it again, and do not let it alias multiple slots.
                guard !self.captures.contains(where: { $0 === candidate }) else {
                    self.notice = "The link did not return an independent source capture. Your reading position is unchanged."
                    return
                }
                self.accept(candidate)
            } catch {
                guard let self else { return }
                if self.operation == ticket, self.epoch == acceptedEpoch, !self.canceling {
                    self.notice = failureMessage(error)
                }
                self.finish(ticket)
            }
        }
    }

    func back() {
        guard canBack else { return }
        position -= 1
        notice = "Earlier linked source restored from retained bytes."
    }
    func forward() {
        guard canForward else { return }
        position += 1
        notice = "Later linked source restored from retained bytes."
    }

    func cancel() {
        guard busy, !canceling else { return }
        epoch = UUID()
        canceling = true
        task?.cancel()
        notice = "Link opening canceled. Your reading position is unchanged; entered work is draining."
    }

    /// A catalog's old page/file receipts cannot cross its replacement.
    /// Retained captures remain usable; the project's ordinary verified-link
    /// path decides whether another destination can be opened from one of them.
    func catalogRefreshed() {
        cancel()
        if current != nil {
            notice = busy
                ? "Project refreshed. The linked sources remain open; the previous link operation is draining."
                : "Project refreshed. Back and Forward keep the retained sources; following another link verifies its source against the current project."
        }
    }

    func close() {
        cancel()
        epoch = UUID()
        scope = nil
        bound = false
        let previous = captures
        captures = []
        position = 0
        for capture in previous { retire(capture) }
        if !busy { notice = "" }
    }

    private func accept(_ candidate: Capture) {
        // A failed or canceled proposal never reaches this point, so Forward
        // remains usable until a new destination has actually been accepted.
        let future = Array(captures.dropFirst(position + 1))
        if position + 1 < captures.count {
            captures.removeSubrange((position + 1)..<captures.count)
        }
        captures.append(candidate)
        position = captures.count - 1
        let excess = captures.count - Self.maximumCaptures
        let expired = excess > 0 ? Array(captures.prefix(excess)) : []
        if excess > 0 {
            captures.removeFirst(excess)
            position -= excess
        }
        // The reader's existing close path cancels its owned operations and
        // retires the capture away from the interaction thread after drain.
        for capture in future { retire(capture) }
        for capture in expired { retire(capture) }
        notice = "Linked source ready. Back and Forward restore retained reading positions."
        if !expired.isEmpty {
            notice += " The oldest linked source was released; the last three remain available."
        }
    }

    private func finish(_ ticket: UUID) {
        guard operation == ticket else { return }
        let wasCanceled = canceling
        operation = nil
        task = nil
        busy = false
        canceling = false
        if wasCanceled {
            notice = bound
                ? "Canceled link work finished. Your retained reading position is unchanged."
                : ""
        }
    }
}
