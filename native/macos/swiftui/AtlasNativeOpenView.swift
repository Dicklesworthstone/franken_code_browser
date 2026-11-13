import SwiftUI
import AppKit

extension AtlasNativeOpen where Reader == AtlasPagedReaderModel {
    static func native() -> AtlasNativeOpen<AtlasPagedReaderModel> {
        AtlasNativeOpen(
            prepare: { try AtlasNativeOpenAccess.selection($0, origin: $1) },
            resolve: { try AtlasNativeOpenAccess.resolve($0, cancellation: $1, lease: $2) },
            capture: { target in
                guard case .file(let url, let lease) = target else { throw AtlasNativeOpenError.invalidSelection }
                let model = AtlasPagedReaderModel(transport: .native, search: .native,
                    outline: .native, document: .native)
                do {
                    // The parent is only a component of the exact file path.
                    // No directory grant, discovery, or local-image source is made.
                    let input = try AtlasReaderInput(root: url.deletingLastPathComponent().path,
                        path: url.lastPathComponent)
                    try await model.openSelectedFile(input: input, accessLease: lease)
                    return model
                } catch {
                    model.close()
                    throw error
                }
            },
            retire: { $0.close() },
            failureMessage: {
                if let error = $0 as? AtlasNativeOpenError { return error.message }
                if let error = $0 as? AtlasReaderError {
                    return error.message + " Selected File captures up to 32 MiB. For larger files, choose a project folder and use its Range Reader."
                }
                return AtlasNativeOpenError.unavailable.message
            })
    }
}

@MainActor struct AtlasNativeOpenStatus: View {
    let model: AtlasNativeOpen<AtlasPagedReaderModel>
    var body: some View {
        if !model.notice.isEmpty {
            HStack(spacing: 10) {
                if model.isLoading { ProgressView().controlSize(.small) }
                Text(model.notice).font(.callout).textSelection(.enabled)
                Spacer(minLength: 8)
                if model.isLoading { Button("Cancel", action: model.cancel) }
                else { Button("Dismiss", action: model.clearNotice) }
            }
            .padding(10)
            .background(.bar)
            .accessibilityElement(children: .contain)
        }
    }
}

@MainActor struct AtlasNativeOpenView: View {
    let model: AtlasNativeOpen<AtlasPagedReaderModel>
    let presentationID: UUID
    let chooseProject: @MainActor () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Selected File").font(.headline)
                    if let url = model.fileURL {
                        Text(String(reflecting: url.path)).font(.caption).textSelection(.enabled)
                    }
                    Text("Only this file is open. Choose a project folder to browse nearby files and local links.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Button("Open Project Folder…", action: chooseProject)
                Button("Close") { model.closeReader(); dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding()
            Divider()
            if let reader = model.reader {
                // No project, local-link callback or image-root authority.
                AtlasPagedSourceView(model: reader).id(ObjectIdentifier(reader))
            }
            AtlasNativeOpenStatus(model: model)
        }
        .frame(minWidth: 1050, minHeight: 640)
        .onDisappear { model.dismissReader(presentationID) }
    }
}

@MainActor struct AtlasNativeOpenCommands: Commands {
    var body: some Commands {
        CommandGroup(after: .newItem) {
            Button("Open…") { AtlasNativeOpenRouting.shared.choose() }
                .keyboardShortcut("o", modifiers: .command)
        }
    }
}

/// Registers an actual native window without replacing SwiftUI's delegate.
@MainActor struct AtlasNativeOpenWindow: NSViewRepresentable {
    let model: AtlasNativeOpen<AtlasPagedReaderModel>
    let newWindow: @MainActor () -> Void
    let receive: @MainActor (AtlasNativeOpenRouting.Event) -> Void

    final class Observer: NSView {
        let registration = UUID()
        private weak var observedWindow: NSWindow?
        private var closeObserver: NSObjectProtocol?
        private var model: AtlasNativeOpen<AtlasPagedReaderModel>?
        private var receive: (@MainActor (AtlasNativeOpenRouting.Event) -> Void)?
        private var installation: Task<Void, Never>?

        override func hitTest(_ point: NSPoint) -> NSView? { nil }
        func configure(model: AtlasNativeOpen<AtlasPagedReaderModel>,
                       newWindow: @escaping @MainActor () -> Void,
                       receive: @escaping @MainActor (AtlasNativeOpenRouting.Event) -> Void) {
            self.model = model; self.receive = receive
            AtlasNativeOpenRouting.shared.requestWindow = newWindow
            attach()
        }
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            attach()
            installation?.cancel()
            installation = Task { @MainActor [weak self] in
                await Task.yield()
                guard !Task.isCancelled else { return }
                self?.attach()
                AtlasNativeOpenRouting.shared.flush(createWindow: false)
                self?.installation = nil
            }
        }
        private func attach() {
            guard let window, let model else { stop(); return }
            if observedWindow !== window {
                stop()
                observedWindow = window
                closeObserver = NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification,
                    object: window, queue: .main) { [weak self] _ in
                        MainActor.assumeIsolated { self?.stop() }
                    }
            }
            AtlasNativeOpenRouting.shared.register(registration,
                available: { [weak self, weak model] in self?.observedWindow != nil && model?.canReceive == true },
                preferred: { [weak window] in
                    guard let window else { return false }
                    return NSApp.keyWindow === window || NSApp.keyWindow?.sheetParent === window || NSApp.mainWindow === window
                },
                receive: { [weak self, weak window] event in
                    window?.makeKeyAndOrderFront(nil)
                    self?.receive?(event)
                })
        }
        func stop() {
            installation?.cancel(); installation = nil
            AtlasNativeOpenRouting.shared.unregister(registration)
            if let closeObserver { NotificationCenter.default.removeObserver(closeObserver) }
            closeObserver = nil; observedWindow = nil
        }
    }

    func makeNSView(context: Context) -> Observer {
        let view = Observer(frame: .zero)
        view.configure(model: model, newWindow: newWindow, receive: receive)
        return view
    }
    func updateNSView(_ view: Observer, context: Context) {
        view.configure(model: model, newWindow: newWindow, receive: receive)
    }
    static func dismantleNSView(_ view: Observer, coordinator: ()) { view.stop() }
}
