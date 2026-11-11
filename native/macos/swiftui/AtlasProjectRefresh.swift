import Foundation
import Observation

/// A replacement catalog is prepared independently of the displayed project.
/// The host retains its accepted browser, readers, search and geometry until
/// the first candidate page has passed the ordinary browser's wire validation.
/// This is an admission/ownership transaction, not filesystem reconciliation:
/// a first page can still have unknown membership and never proves deletion.
@Observable @MainActor final class AtlasProjectRefresh {
    struct Candidate {
        let browser: AtlasProjectBrowserModel
        let opening: AtlasProjectOpening
        let project: UUID
        let previousProject: UUID
        let root: String
        let accessIdentity: ObjectIdentifier?
    }

    @MainActor private final class Attempt {
        let browser: AtlasProjectBrowserModel
        let ticket: AtlasProjectOpening.Ticket
        let previousProject: UUID
        let root: String
        let accessIdentity: ObjectIdentifier?
        let failed: @MainActor (UUID) -> Void
        var opening: AtlasProjectOpening
        var canceled = false
        private var failureDelivered = false

        init(browser: AtlasProjectBrowserModel, project: UUID, root: String,
             accessIdentity: ObjectIdentifier?, failed: @escaping @MainActor (UUID) -> Void) {
            self.browser = browser
            self.previousProject = project
            self.root = root
            self.accessIdentity = accessIdentity
            self.failed = failed
            var opening = AtlasProjectOpening()
            ticket = opening.begin()
            self.opening = opening
        }

        func failOnce() {
            guard !failureDelivered else { return }
            failureDelivered = true
            failed(ticket.project)
        }
    }

    private(set) var isRefreshing = false
    private(set) var notice = ""
    @ObservationIgnored private let transport: AtlasProjectBrowserTransport
    @ObservationIgnored private var attempt: Attempt?
    @ObservationIgnored private var task: Task<Void, Never>?

    init(transport: AtlasProjectBrowserTransport) { self.transport = transport }

    /// Returns the candidate discovery identity synchronously. The host can
    /// register its watcher scan before the main-actor task begins. There is
    /// only one candidate, including canceled work still draining a foreign
    /// operation; rapid retries cannot accumulate catalogs or native grants.
    ///
    /// `accepted` runs synchronously after validation. The host must compare
    /// its current project/browser/root/grant with this attempt and install the
    /// replacement without suspension. Returning true transfers browser
    /// ownership; false retires only the candidate. Failure/cancel reports its
    /// discovery identity once so the host can withdraw that acknowledgment.
    @discardableResult
    func stage(root: String, project: UUID, accessLease: AnyObject?,
               accepted: @escaping @MainActor (Candidate) -> Bool,
               failed: @escaping @MainActor (UUID) -> Void) -> UUID? {
        guard !isRefreshing, root.hasPrefix("/"), root.utf8.count <= 16_384,
              !root.utf8.contains(0) else { return nil }
        let browser = AtlasProjectBrowserModel(transport: transport)
        let request = Attempt(browser: browser, project: project, root: root,
            accessIdentity: accessLease.map(ObjectIdentifier.init), failed: failed)
        attempt = request
        isRefreshing = true
        notice = "Checking the project… Your current files, readers and search remain available."
        task = Task { [weak self] in
            var transferred = false
            defer {
                if !transferred { browser.close() }
                self?.finish(request)
            }
            do {
                try Task.checkCancellation()
                try await browser.open(root: root, accessLease: accessLease)
                try Task.checkCancellation()
                guard let self, self.attempt === request, !request.canceled,
                      !browser.closed, browser.currentPage, browser.info != nil,
                      browser.root.utf8.elementsEqual(root.utf8),
                      request.opening.acceptProgressive(for: request.ticket) else {
                    request.failOnce()
                    return
                }
                let candidate = Candidate(browser: browser, opening: request.opening,
                    project: request.ticket.project, previousProject: request.previousProject,
                    root: request.root, accessIdentity: request.accessIdentity)
                // There is deliberately no await between qualification and the
                // host's installation of this exact browser/opening identity.
                if accepted(candidate) {
                    transferred = true
                    self.notice = "Project refreshed. Retained source captures keep their original bytes."
                } else {
                    request.failOnce()
                    self.notice = "Refresh was not applied because the active project changed."
                }
            } catch {
                request.failOnce()
                if let self, self.attempt === request, !request.canceled {
                    self.notice = "Refresh failed. Your previous project remains available."
                }
            }
        }
        return request.ticket.project
    }

    /// Revoke publication immediately. Closing the browser requests bounded
    /// cancellation; entered native work retains its own session/access lease
    /// until it drains. The occupied slot is released only by finish().
    func cancel() {
        guard let attempt, !attempt.canceled else { return }
        attempt.canceled = true
        task?.cancel()
        attempt.browser.close()
        attempt.failOnce()
        notice = "Refresh canceled. Your previous project remains available."
    }

    /// Reusable on window disappearance; a later appearance may admit a new
    /// refresh after any entered work has drained.
    func close() { cancel() }

    private func finish(_ request: Attempt) {
        guard attempt === request else { return }
        attempt = nil
        task = nil
        isRefreshing = false
    }
}
