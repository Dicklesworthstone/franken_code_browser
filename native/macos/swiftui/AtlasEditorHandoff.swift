import Foundation
import Observation

enum AtlasEditorError: Error {
    case invalidPath, invalidApplication, unavailable
    var message: String {
        switch self {
        case .invalidPath: return "This source has no safe current-file path for an external editor."
        case .invalidApplication: return "Choose a local application bundle to use as your editor."
        case .unavailable: return "The editor could not open the current disk file. Check that the file and application are still available."
        }
    }
}

/// Native URLs are structured values, never a shell command or an editor's
/// guessed command-line grammar. This admits an already-authorized path; it
/// does not turn a display label, retained capture or URI into file authority.
enum AtlasEditorPath {
    static func file(_ path: String, application: Bool = false) throws -> URL {
        guard path.utf8.count <= 16_384, path.utf8.first == 47, path.utf8.count > 1,
              !path.unicodeScalars.contains(where: { scalar in
                  let v = scalar.value
                  return v < 32 || (127...159).contains(v) || v == 0x061c
                      || (0x200e...0x200f).contains(v) || (0x2028...0x202e).contains(v)
                      || (0x2066...0x206f).contains(v)
              }) else { throw AtlasEditorError.invalidPath }
        let components = path.utf8.dropFirst().split(separator: 47, omittingEmptySubsequences: false)
        guard components.count <= 256,
              !components.contains(where: { $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46]) }) else {
            throw AtlasEditorError.invalidPath
        }
        if application && !path.lowercased().hasSuffix(".app") { throw AtlasEditorError.invalidApplication }
        let url = URL(fileURLWithPath: path, isDirectory: application)
        guard url.path.utf8.elementsEqual(path.utf8), lossless(url, path: path) else {
            throw AtlasEditorError.invalidPath
        }
        return url
    }

    static func application(_ selected: URL) throws -> URL {
        guard selected.isFileURL, selected.host == nil || selected.host == "" || selected.host == "localhost",
              selected.query == nil, selected.fragment == nil, selected.user == nil,
              selected.password == nil, selected.port == nil,
              lossless(selected, path: selected.path) else { throw AtlasEditorError.invalidApplication }
        return try file(selected.path, application: true)
    }

    private static func lossless(_ url: URL, path: String) -> Bool {
        guard path.utf8.count <= 16_384 else { return false }
        let bytes = Array(path.utf8)
        return url.withUnsafeFileSystemRepresentation { pointer in
            guard let pointer else { return false }
            for index in bytes.indices {
                guard pointer[index] != 0, UInt8(bitPattern: pointer[index]) == bytes[index] else { return false }
            }
            return pointer[bytes.count] == 0
        }
    }
}

@MainActor struct AtlasEditorSource {
    let url: URL
    let accessLease: AnyObject?
    let isCurrent: @MainActor () -> Bool
    init(path: String, accessLease: AnyObject?, isCurrent: @escaping @MainActor () -> Bool) throws {
        url = try AtlasEditorPath.file(path)
        self.accessLease = accessLease; self.isCurrent = isCurrent
    }
}

@MainActor struct AtlasEditorApplication {
    let url: URL
    let accessLease: AnyObject?
    init(selected: URL, accessLease: AnyObject? = nil) throws {
        url = try AtlasEditorPath.application(selected)
        self.accessLease = accessLease
    }
    var name: String { url.deletingPathExtension().lastPathComponent }
}

/// Only an explicit native application choice configures the target. Session
/// scope avoids restoring a path label as a sandbox grant on the next launch.
@Observable @MainActor final class AtlasEditorConfiguration {
    static let shared = AtlasEditorConfiguration()
    var application: AtlasEditorApplication?
    func forget() { application = nil }
}

/// One action stays admitted through chooser/metadata/OS completion. Cancel
/// withdraws an unsubmitted intent but cannot revoke an OS action already sent.
/// A canceled chooser must drain before another action can enter this lane.
@Observable @MainActor final class AtlasEditorHandoff {
    enum Phase { case idle, choosing, opening }
    private(set) var phase: Phase = .idle
    private(set) var notice = ""
    let configuration: AtlasEditorConfiguration
    @ObservationIgnored private let choose: @MainActor () async throws -> AtlasEditorApplication?
    @ObservationIgnored private let cancelChoice: @MainActor () -> Void
    @ObservationIgnored private let submit: @MainActor (URL, URL) async throws -> Void
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var generation = UUID()
    var busy: Bool { phase != .idle }
    var canCancel: Bool { phase == .choosing }

    init(configuration: AtlasEditorConfiguration,
         choose: @escaping @MainActor () async throws -> AtlasEditorApplication?,
         cancelChoice: @escaping @MainActor () -> Void = {},
         submit: @escaping @MainActor (URL, URL) async throws -> Void) {
        self.configuration = configuration; self.choose = choose
        self.cancelChoice = cancelChoice; self.submit = submit
    }

    func open(_ source: AtlasEditorSource, chooseAnother: Bool = false) {
        guard !busy else { return }
        guard source.isCurrent() else { notice = "The originating reader has changed. Choose the current file again."; return }
        generation = UUID()
        let ticket = generation
        phase = .choosing; notice = "Preparing to open the current disk file…"
        task = Task { [self, source] in
            var application: AtlasEditorApplication?
            defer {
                // Neither a closed reader nor Forget Editor releases grants
                // while an entered foreign operation still uses its inputs.
                withExtendedLifetime(source.accessLease) {}
                withExtendedLifetime(application?.accessLease) {}
                if phase == .opening && !source.isCurrent() { notice = "" }
                phase = .idle; task = nil
            }
            do {
                guard ticket == generation, !Task.isCancelled else { return }
                guard source.isCurrent() else {
                    notice = "The originating reader has changed. No file was sent to the editor."; return
                }
                if chooseAnother || configuration.application == nil { application = try await choose() }
                else { application = configuration.application }
                guard ticket == generation, !Task.isCancelled else { return }
                guard source.isCurrent() else {
                    notice = "The originating reader has changed. No file was sent to the editor."; return
                }
                guard let application else { notice = "Editor choice canceled."; return }
                configuration.application = application
                // No actor suspension separates this final authority check from
                // entering the structured OS submission below.
                phase = .opening; notice = "Opening the current disk file in \(application.name)…"
                try await submit(application.url, source.url)
                guard ticket == generation, source.isCurrent() else { return }
                notice = "Sent the current disk file to \(application.name). Retained source bytes are unchanged."
            } catch {
                guard ticket == generation, !Task.isCancelled else { return }
                notice = (error as? AtlasEditorError)?.message ?? AtlasEditorError.unavailable.message
            }
        }
    }

    func cancel() {
        guard canCancel else { return }
        generation = UUID(); task?.cancel(); cancelChoice()
        notice = "Editor action canceled. No file was sent."
    }
    func clearNotice() { if !busy { notice = "" } }
}
