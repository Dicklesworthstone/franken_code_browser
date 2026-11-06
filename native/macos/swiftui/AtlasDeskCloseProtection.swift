import Foundation

enum AtlasDeskCloseChoice: Equatable, Sendable { case keepWorking, review, discard }
enum AtlasDeskCloseReason: Equatable, Sendable { case window, application }
struct AtlasDeskClosePrompt: Sendable {
    let window: UUID
    let title: String
    let reason: AtlasDeskCloseReason
    let snapshot: AtlasDeskCloseSnapshot
    var canDiscard: Bool { !snapshot.busy }
}

/// One synchronous decision boundary for window close and application quit.
/// It performs no bridge calls or filesystem work. Reviews leave the window
/// open; saving never schedules a later automatic close of newly edited work.
@MainActor final class AtlasDeskCloseCoordinator {
    static let shared = AtlasDeskCloseCoordinator()
    private struct Entry {
        let title: @MainActor () -> String
        let snapshot: @MainActor () -> AtlasDeskCloseSnapshot
        let focus: @MainActor () -> Void
        let review: @MainActor () -> Void
    }
    private var entries: [UUID: Entry] = [:]
    private var order: [UUID] = []
    private var reviewing = false

    func register(_ id: UUID, title: @escaping @MainActor () -> String,
        snapshot: @escaping @MainActor () -> AtlasDeskCloseSnapshot,
        focus: @escaping @MainActor () -> Void = {}, review: @escaping @MainActor () -> Void) {
        if entries[id] == nil { order.append(id) }
        entries[id] = Entry(title: title, snapshot: snapshot, focus: focus, review: review)
    }
    func unregister(_ id: UUID) {
        entries.removeValue(forKey: id); order.removeAll { $0 == id }
    }

    func shouldClose(_ id: UUID, choose: @MainActor (AtlasDeskClosePrompt) -> AtlasDeskCloseChoice) -> Bool {
        guard !reviewing else { return false }
        guard let entry = entries[id] else { return true }
        reviewing = true
        defer { reviewing = false }
        let before = entry.snapshot()
        guard before.requiresReview else { return true }
        entry.focus()
        switch choose(AtlasDeskClosePrompt(window: id, title: entry.title(), reason: .window, snapshot: before)) {
        case .keepWorking: return false
        case .review: entry.review(); return false
        case .discard:
            // An entered save/mutation must reach its actual terminal outcome.
            // AppKit's nested modal loop can deliver newer state or draft edits
            // while the prompt is open, invalidating this particular approval.
            return !before.busy && entries[id]?.snapshot() == before
        }
    }

    func shouldTerminate(choose: @MainActor (AtlasDeskClosePrompt) -> AtlasDeskCloseChoice) -> Bool {
        guard !reviewing else { return false }
        reviewing = true
        defer { reviewing = false }
        let windows = order
        var approved: [UUID: AtlasDeskCloseSnapshot] = [:]
        for id in windows {
            guard let entry = entries[id] else { return false }
            let before = entry.snapshot()
            approved[id] = before
            guard before.requiresReview else { continue }
            entry.focus()
            switch choose(AtlasDeskClosePrompt(window: id, title: entry.title(), reason: .application, snapshot: before)) {
            case .keepWorking: return false
            case .review: entry.review(); return false
            case .discard:
                guard !before.busy, entries[id]?.snapshot() == before else { return false }
            }
        }
        // Nothing is discarded while collecting decisions. Canceling any
        // window's review cancels the entire quit, with no remembered waiver.
        // Also recheck windows which were clean before another modal appeared.
        guard order == windows else { return false }
        return windows.allSatisfy { id in
            guard let entry = entries[id], let snapshot = approved[id] else { return false }
            return entry.snapshot() == snapshot
        }
    }
}

#if canImport(AppKit) && canImport(SwiftUI)
import AppKit
import SwiftUI

@MainActor private enum AtlasDeskCloseAlert {
    static func choose(_ prompt: AtlasDeskClosePrompt) -> AtlasDeskCloseChoice {
        let alert = NSAlert()
        let quitting = prompt.reason == .application
        alert.alertStyle = .warning
        alert.messageText = quitting ? "Keep your reading desk before quitting?" : "Keep your reading desk before closing?"
        let detail: String
        if prompt.snapshot.busy {
            detail = "A reading desk operation is still running. Keep this window open until its result is known. Review the desk to finish or cancel the request."
        } else if !prompt.snapshot.drafts.isEmpty {
            detail = "Your note editor has unsaved text or an unconfirmed note submission. Review the draft, keep it in the desk, then save a new checkpoint. A checkpoint does not include unsubmitted draft text."
        } else if prompt.snapshot.unverifiedChanges {
            detail = "The last reading desk change could not be verified. Review and reload its accepted state before saving a checkpoint. Closing now may lose personal notes or retained sources."
        } else {
            detail = "Changes to retained sources, bookmarks, notes, or reading locations have not been saved in a checkpoint. Review the desk and save a new checkpoint to keep them."
        }
        alert.informativeText = "\(prompt.title)\n\n\(detail)"
        alert.addButton(withTitle: "Keep Working")
        alert.addButton(withTitle: "Review Reading Desk")
        if prompt.canDiscard {
            let discard = alert.addButton(withTitle: quitting ? "Discard and Quit" : "Discard and Close")
            discard.hasDestructiveAction = true
            discard.keyEquivalent = ""
        }
        // Return always chooses Keep Working. There is no default destructive
        // action, even when this window is the last remaining application UI.
        alert.buttons[0].keyEquivalent = "\r"
        alert.buttons[1].keyEquivalent = ""
        switch alert.runModal() {
        case .alertSecondButtonReturn: return .review
        case .alertThirdButtonReturn where prompt.canDiscard: return .discard
        default: return .keepWorking
        }
    }
}

@MainActor final class AtlasDeskApplicationDelegate: NSObject, NSApplicationDelegate {
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        AtlasDeskCloseCoordinator.shared.shouldTerminate(choose: AtlasDeskCloseAlert.choose)
            ? .terminateNow : .terminateCancel
    }
}

/// SwiftUI remains the owner of its window delegate. This adapter interposes
/// only the close decision and forwards the original delegate's other optional
/// selectors, including its window restoration and scene lifecycle callbacks.
@MainActor private final class AtlasDeskWindowCloseDelegate: NSObject, NSWindowDelegate {
    weak var previous: (any NSWindowDelegate)?
    weak var observer: AtlasDeskCloseProtection.Observer?
    let registration: UUID
    let coordinator: AtlasDeskCloseCoordinator
    init(previous: (any NSWindowDelegate)?, observer: AtlasDeskCloseProtection.Observer,
        registration: UUID, coordinator: AtlasDeskCloseCoordinator) {
        self.previous = previous; self.observer = observer
        self.registration = registration; self.coordinator = coordinator
    }
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        if previous?.windowShouldClose?(sender) == false { return false }
        return coordinator.shouldClose(registration, choose: AtlasDeskCloseAlert.choose)
    }
    func windowWillClose(_ notification: Notification) {
        previous?.windowWillClose?(notification)
        observer?.stop()
    }
    nonisolated override func responds(to selector: Selector!) -> Bool {
        if super.responds(to: selector) { return true }
        // AppKit window delegate inspection and delivery run on the main
        // thread. Keep Objective-C's synchronous forwarding on that executor.
        return MainActor.assumeIsolated { previous?.responds(to: selector) ?? false }
    }
    private struct ForwardTarget: @unchecked Sendable { let value: (any NSWindowDelegate)? }
    nonisolated override func forwardingTarget(for selector: Selector!) -> Any? {
        let target = MainActor.assumeIsolated {
            ForwardTarget(value: previous?.responds(to: selector) == true ? previous : nil)
        }
        if let delegate = target.value { return delegate }
        return super.forwardingTarget(for: selector)
    }
}

@MainActor struct AtlasDeskCloseProtection: NSViewRepresentable {
    let model: AtlasReadingDeskModel
    let review: @MainActor () -> Void

    final class Observer: NSView {
        private var model: AtlasReadingDeskModel?
        private var review: @MainActor () -> Void = {}
        private weak var observedWindow: NSWindow?
        private var proxy: AtlasDeskWindowCloseDelegate?
        private var installation: Task<Void, Never>?
        private let coordinator = AtlasDeskCloseCoordinator.shared

        override func hitTest(_ point: NSPoint) -> NSView? { nil }
        func configure(model: AtlasReadingDeskModel, review: @escaping @MainActor () -> Void) {
            self.model = model; self.review = review
            attach()
        }
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            installation?.cancel()
            attach()
            guard window != nil else { return }
            // SwiftUI may assign its own delegate during initial window setup.
            // Recheck once after setup, preserving that final delegate as well.
            installation = Task { @MainActor [weak self] in
                await Task.yield()
                guard !Task.isCancelled else { return }
                self?.attach(); self?.installation = nil
            }
        }
        private func attach() {
            guard let window else { stop(); return }
            if observedWindow === window, let proxy {
                if window.delegate !== proxy { proxy.previous = window.delegate; window.delegate = proxy }
                return
            }
            stop()
            let registration = UUID()
            let proxy = AtlasDeskWindowCloseDelegate(previous: window.delegate, observer: self,
                registration: registration, coordinator: coordinator)
            observedWindow = window; self.proxy = proxy
            coordinator.register(registration, title: { [weak window] in window?.title ?? "Reading desk" },
                snapshot: { [weak self] in self?.model?.closeSnapshot ?? .empty },
                focus: { [weak window] in window?.makeKeyAndOrderFront(nil) },
                review: { [weak self] in self?.reviewDesk() })
            window.delegate = proxy
        }
        private func reviewDesk() {
            observedWindow?.makeKeyAndOrderFront(nil)
            if model?.hasOpenNoteEditor == true {
                // Never replace an attached note editor with another sheet.
                // Its draft must first be kept or explicitly discarded there.
                var sheet = observedWindow?.attachedSheet
                while let child = sheet?.attachedSheet { sheet = child }
                sheet?.makeKeyAndOrderFront(nil)
            } else { review() }
        }
        func stop() {
            installation?.cancel(); installation = nil
            if let proxy {
                coordinator.unregister(proxy.registration)
                if observedWindow?.delegate === proxy { observedWindow?.delegate = proxy.previous }
            }
            proxy = nil; observedWindow = nil
        }
    }

    func makeNSView(context: Context) -> Observer {
        let view = Observer(frame: .zero)
        view.configure(model: model, review: review)
        return view
    }
    func updateNSView(_ view: Observer, context: Context) { view.configure(model: model, review: review) }
    static func dismantleNSView(_ view: Observer, coordinator: ()) { view.stop() }
}
#endif
