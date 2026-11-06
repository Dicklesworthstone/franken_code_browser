import Foundation
#if canImport(AppKit) && canImport(SwiftUI)
import AppKit
import SwiftUI
#endif

private func state(owner: UInt64 = 11, revision: UInt64 = 1, dirty: Bool = true,
    busy: Bool = false, unknown: Bool = false, drafts: Set<AtlasDeskDraftRevision> = []) -> AtlasDeskCloseSnapshot {
    AtlasDeskCloseSnapshot(owner: owner, revision: revision, busy: busy,
        unsavedContents: dirty, unverifiedChanges: unknown, drafts: drafts)
}

@MainActor private final class DeskWindow {
    let id = UUID()
    var snapshot: AtlasDeskCloseSnapshot
    var reviews = 0
    var focuses = 0
    init(_ snapshot: AtlasDeskCloseSnapshot = state()) { self.snapshot = snapshot }
    func register(in coordinator: AtlasDeskCloseCoordinator) {
        coordinator.register(id, title: { "Fixture reading desk" }, snapshot: { [self] in snapshot },
            focus: { [self] in focuses += 1 }, review: { [self] in reviews += 1 })
    }
}

/// fcb-yyh.1: production close policy with injected modal choices. These tests
/// do not pretend an injected checkpoint response is a real filesystem write.
@main struct AtlasDeskCloseProtectionTests {
    @MainActor static func main() async {
        closeChoices()
        changedApproval()
        terminationAcrossWindows()
        changedTermination()
#if canImport(AppKit) && canImport(SwiftUI)
        if CommandLine.arguments.contains("--native-window") { await nativeDelegateForwarding() }
#else
        precondition(!CommandLine.arguments.contains("--native-window"), "native window regression requires macOS")
#endif
        print("Desk close policy regressions passed; native qualification and checkpoint I/O are separate.")
    }

    @MainActor static func closeChoices() {
        let coordinator = AtlasDeskCloseCoordinator(), window = DeskWindow(.empty)
        window.register(in: coordinator)
        precondition(coordinator.shouldClose(window.id) { _ in fatalError("a clean desk prompted") })
        window.snapshot = state()
        precondition(!coordinator.shouldClose(window.id) { prompt in
            precondition(prompt.reason == .window && prompt.canDiscard && prompt.window == window.id)
            return .keepWorking
        })
        precondition(!coordinator.shouldClose(window.id) { _ in .review } && window.reviews == 1)
        precondition(coordinator.shouldClose(window.id) { _ in .discard })
        precondition(window.snapshot.unsavedContents, "approval must not mutate or discard the desk itself")
        precondition(!coordinator.shouldClose(window.id) { _ in .keepWorking },
            "a canceled close has no remembered discard waiver")

        window.snapshot = state(busy: true)
        precondition(!coordinator.shouldClose(window.id) { prompt in
            precondition(!prompt.canDiscard)
            return .discard // Negative control: even a broken UI cannot approve an active write.
        })
        window.snapshot = state(dirty: false, unknown: true)
        precondition(!coordinator.shouldClose(window.id) { _ in .keepWorking },
            "an unknown mutation outcome must not look like a clean empty desk")
        window.snapshot = state()
        precondition(!coordinator.shouldClose(window.id) { _ in
            precondition(!coordinator.shouldClose(window.id) { _ in fatalError("reentrant prompt") })
            precondition(!coordinator.shouldTerminate { _ in fatalError("reentrant quit") })
            return .keepWorking
        })
        coordinator.unregister(window.id)
        precondition(coordinator.shouldClose(window.id) { _ in fatalError("an unregistered window prompted") })
    }

    @MainActor static func changedApproval() {
        let coordinator = AtlasDeskCloseCoordinator(), window = DeskWindow()
        window.register(in: coordinator)
        precondition(!coordinator.shouldClose(window.id) { _ in
            window.snapshot = state(revision: 2)
            return .discard
        }, "a newly completed desk mutation invalidates approval for the old revision")
        precondition(!coordinator.shouldClose(window.id) { _ in
            window.snapshot = state(owner: 12, revision: 2)
            return .discard
        }, "another desk owner cannot inherit a discard decision")

        let draft = UUID()
        window.snapshot = state(dirty: false, drafts: [AtlasDeskDraftRevision(id: draft, revision: UUID())])
        precondition(!coordinator.shouldClose(window.id) { _ in
            window.snapshot = state(dirty: false, drafts: [AtlasDeskDraftRevision(id: draft, revision: UUID())])
            return .discard
        }, "a newer note edit invalidates discard even when the saved desk revision is unchanged")
        precondition(!coordinator.shouldClose(window.id) { _ in
            window.snapshot = .empty
            return .review
        }, "Review never automatically closes a window, including after a save finishes")
        precondition(coordinator.shouldClose(window.id) { _ in fatalError("clean retry prompted") })
    }

    @MainActor static func terminationAcrossWindows() {
        let coordinator = AtlasDeskCloseCoordinator(), first = DeskWindow(), second = DeskWindow(state(owner: 22))
        first.register(in: coordinator); second.register(in: coordinator)
        var prompts = 0
        precondition(!coordinator.shouldTerminate { prompt in
            prompts += 1
            precondition(prompt.reason == .application)
            return prompt.window == first.id ? .discard : .keepWorking
        })
        precondition(prompts == 2 && first.snapshot.unsavedContents && second.snapshot.unsavedContents)
        prompts = 0
        precondition(coordinator.shouldTerminate { _ in prompts += 1; return .discard })
        precondition(prompts == 2, "every quit attempt must review every dirty window again")
        precondition(!coordinator.shouldTerminate { prompt in
            prompt.window == second.id ? .review : .discard
        })
        precondition(second.reviews == 1)
        first.snapshot = state(busy: true)
        precondition(!coordinator.shouldTerminate { prompt in
            precondition(!prompt.canDiscard)
            return .discard
        }, "quit must not interrupt an entered checkpoint write")
    }

    @MainActor static func changedTermination() {
        let coordinator = AtlasDeskCloseCoordinator(), first = DeskWindow(state(dirty: false)), second = DeskWindow(state(owner: 22))
        first.register(in: coordinator); second.register(in: coordinator)
        precondition(!coordinator.shouldTerminate { prompt in
            precondition(prompt.window == second.id)
            first.snapshot = state(revision: 2)
            return .discard
        }, "a window that became dirty during another review cannot be skipped")
        precondition(!coordinator.shouldTerminate { prompt in
            if prompt.window == second.id {
                first.snapshot = state(revision: 3,
                    drafts: [AtlasDeskDraftRevision(id: UUID(), revision: UUID())])
            }
            return .discard
        }, "earlier approved windows must be rechecked after the final modal")
        let arriving = DeskWindow(state(owner: 33))
        precondition(!coordinator.shouldTerminate { prompt in
            if prompt.window == second.id { arriving.register(in: coordinator) }
            return .discard
        }, "a new window appearing during review cancels that quit attempt")
        coordinator.unregister(arriving.id)
        precondition(!coordinator.shouldTerminate { prompt in
            if prompt.window == second.id { coordinator.unregister(first.id) }
            return .discard
        }, "registry changes cannot reuse an approval collected for a different set of windows")
    }

#if canImport(AppKit) && canImport(SwiftUI)
    /// Optional AppKit fixture: verify real Objective-C forwarding and window
    /// delegate installation. It is not rendering or physical-Mac qualification.
    @MainActor static func nativeDelegateForwarding() async {
        let application = NSApplication.shared
        let model = AtlasReadingDeskModel(transport: AtlasReadingDeskTransport(
            create: { fatalError("close inspection must not start a desk") },
            run: { _, _ in fatalError("close inspection must not call the bridge") },
            cancel: { _ in fatalError("clean close must not cancel work") }, close: { _ in true },
            retirementFailed: { fatalError("an inert desk must not need retirement") }))
        let original = WindowDelegate()
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 320, height: 240),
            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.delegate = original
        let observer = AtlasDeskCloseProtection.Observer(frame: .zero)
        observer.configure(model: model, review: { fatalError("a clean window must not need review") })
        window.contentView = observer
        guard let proxy = window.delegate else { fatalError("close delegate was not installed") }
        precondition(proxy !== original)
        precondition(proxy.windowShouldClose?(window) == true && original.closeQueries == 1)
        original.allowClose = false
        precondition(proxy.windowShouldClose?(window) == false && original.closeQueries == 2,
            "the original SwiftUI delegate retains its veto")
        original.allowClose = true
        let resized = original.resizes
        precondition(proxy.responds(to: #selector(NSWindowDelegate.windowDidResize(_:))))
        proxy.windowDidResize?(Notification(name: NSWindow.didResizeNotification, object: window))
        precondition(original.resizes == resized + 1, "unhandled lifecycle selectors must be forwarded")
        precondition(AtlasDeskApplicationDelegate().applicationShouldTerminate(application) == .terminateNow)
        observer.stop()
        await Task.yield()
        precondition(window.delegate === original, "detaching restores SwiftUI's delegate")
        observer.configure(model: model, review: {})
        let priorClose = original.willCloses
        window.performClose(nil)
        precondition(original.willCloses == priorClose + 1 && window.delegate === original,
            "closing forwards lifecycle delivery and unregisters the desk")
        model.close()
        print("AppKit close delegate forwarding regression passed.")
    }

    @MainActor private final class WindowDelegate: NSObject, NSWindowDelegate {
        var allowClose = true
        var closeQueries = 0
        var resizes = 0
        var willCloses = 0
        func windowShouldClose(_ sender: NSWindow) -> Bool { closeQueries += 1; return allowClose }
        func windowDidResize(_ notification: Notification) { resizes += 1 }
        func windowWillClose(_ notification: Notification) { willCloses += 1 }
    }
#endif
}
