import SwiftUI
import AppKit
import UniformTypeIdentifiers

/// This adapter reads only the model's accepted live-file capability. Saved
/// search/desk labels and an unbound retained capture never enable the action.
extension AtlasPagedReaderModel {
    func externalEditorSource() -> AtlasEditorSource? {
        guard let admitted = currentFileForExternalEditor() else { return nil }
        return try? AtlasEditorSource(path: admitted.input.fullPath, accessLease: admitted.lease,
            isCurrent: { [weak self] in
                guard let current = self?.currentFileForExternalEditor() else { return false }
                return current.identity.matches(admitted.identity)
                    && current.input.fullPath.utf8.elementsEqual(admitted.input.fullPath.utf8)
                    && current.lease === admitted.lease
            })
    }
}

@MainActor private final class AtlasEditorChooser {
    private var panel: NSOpenPanel?
    private let io = AtlasProjectIO()
    private var cancellation: AtlasSearchCancellation?

    func choose() async throws -> AtlasEditorApplication? {
        let panel = NSOpenPanel()
        panel.title = "Choose an Editor"
        panel.message = "This application will open the current file on disk. Its contents may differ from the retained source. The editor is remembered for this session."
        panel.prompt = "Use Editor"
        panel.canChooseFiles = true; panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false; panel.treatsFilePackagesAsDirectories = false
        panel.allowedContentTypes = [.applicationBundle]
        panel.directoryURL = URL(fileURLWithPath: "/Applications", isDirectory: true)
        self.panel = panel
        let accepted = await withCheckedContinuation { (reply: CheckedContinuation<Bool, Never>) in
            panel.begin { response in reply.resume(returning: response == .OK) }
        }
        self.panel = nil
        guard accepted, let url = panel.url else { return nil }
        let lease: AnyObject?
#if FCB_APP_STORE
        // Adopt the panel's exact application selection, not its parent folder.
        guard let selected = AppStoreRootAccess.selectionRequest(url) else { throw AtlasEditorError.invalidApplication }
        lease = selected.1
#else
        lease = nil
#endif
        let application = try AtlasEditorApplication(selected: url, accessLease: lease)
        let flag = AtlasSearchCancellation(); cancellation = flag
        defer { cancellation = nil }
        let candidate = application.url
        // Bundle metadata can block on a volume. Never inspect it on the UI
        // actor, and retain the panel's grant until that inspection drains.
        let _: Bool = try await io.perform(cancellation: flag, keepingAlive: lease.map { [$0] } ?? []) {
            let values = try candidate.resourceValues(forKeys: [.isApplicationKey, .isDirectoryKey, .isSymbolicLinkKey])
            guard values.isApplication == true, values.isDirectory == true, values.isSymbolicLink != true else {
                throw AtlasEditorError.invalidApplication
            }
            return true
        }
        return application
    }

    func cancel() { panel?.cancel(nil); cancellation?.cancel(); io.cancel() }
}

extension AtlasEditorHandoff {
    static func native() -> AtlasEditorHandoff {
        let chooser = AtlasEditorChooser()
        return AtlasEditorHandoff(configuration: .shared, choose: { try await chooser.choose() },
            cancelChoice: { chooser.cancel() }, submit: { application, source in
                let configuration = NSWorkspace.OpenConfiguration()
                configuration.activates = true
                configuration.allowsRunningApplicationSubstitution = false
                // URLs remain separate through the real OS boundary: no shell,
                // command arguments, URI decoding or inferred line coordinates.
                try await withCheckedThrowingContinuation { (reply: CheckedContinuation<Void, Error>) in
                    NSWorkspace.shared.open([source], withApplicationAt: application, configuration: configuration) { _, error in
                        if error != nil { reply.resume(throwing: AtlasEditorError.unavailable) }
                        else { reply.resume() }
                    }
                }
            })
    }
}

@MainActor struct AtlasEditorActionView: View {
    let source: @MainActor () -> AtlasEditorSource?
    @State private var handoff = AtlasEditorHandoff.native()

    var body: some View {
        HStack(spacing: 4) {
            Button("Open Current File in Editor…") { open() }
                .disabled(handoff.busy || source() == nil)
                .help(source() == nil
                    ? "Choose this file with Open to enable external editing. Saved source alone does not provide access to the current disk file."
                    : "Open the current disk file in your chosen editor. The file may differ from this retained source; no captured line or column is passed.")
            Menu {
                if let application = handoff.configuration.application {
                    Text("Editor: \(application.name)")
                }
                Button("Choose Another Editor…") { open(chooseAnother: true) }
                    .disabled(source() == nil)
                Button("Forget Editor", action: handoff.configuration.forget)
                    .disabled(handoff.configuration.application == nil)
            } label: { Image(systemName: "chevron.down") }
                .menuStyle(.borderlessButton).fixedSize()
                .disabled(handoff.busy).accessibilityLabel("External editor options")
            if handoff.busy { ProgressView().controlSize(.small) }
            if handoff.canCancel { Button("Cancel editor action", action: handoff.cancel) }
            if !handoff.notice.isEmpty {
                Text(handoff.notice).font(.caption).lineLimit(1).frame(maxWidth: 240)
                    .help(handoff.notice).accessibilityLabel(handoff.notice)
                if !handoff.busy {
                    Button(action: handoff.clearNotice) { Image(systemName: "xmark.circle") }
                        .buttonStyle(.borderless).accessibilityLabel("Dismiss editor status")
                }
            }
        }
        .onDisappear { handoff.cancel() }
    }

    private func open(chooseAnother: Bool = false) {
        guard let current = source() else { return }
        handoff.open(current, chooseAnother: chooseAnother)
    }
}
