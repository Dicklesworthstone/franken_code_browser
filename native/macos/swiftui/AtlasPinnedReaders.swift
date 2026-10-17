import Foundation
import Observation

/// Bounded native ownership of independently forked readers. Source semantics,
/// byte admission and capture identity stay in the retained Rust reader. The
/// injected operations let lifecycle tests exercise this host without AppKit.
@Observable @MainActor final class AtlasPinnedReaders<Reader: AnyObject> {
    struct Entry: Identifiable {
        let id: UUID
        let label: String
        let reader: Reader
    }

    // Leave room in the eight-handle Rust registry for the current reader,
    // replacement proposals and foreign operations that are still draining.
    static var maximumPins: Int { 2 }
    private(set) var entries: [Entry] = []
    private(set) var selectedID: UUID?
    private(set) var busy = false
    private(set) var notice = ""
    @ObservationIgnored private var root: String?
    @ObservationIgnored private var operation = UUID()
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private let canFork: @MainActor (Reader) -> Bool
    @ObservationIgnored private let fork: @MainActor (Reader) async throws -> Reader
    @ObservationIgnored private let retire: @MainActor (Reader) -> Void
    @ObservationIgnored private let failureMessage: @MainActor (Error) -> String

    init(canFork: @escaping @MainActor (Reader) -> Bool,
         fork: @escaping @MainActor (Reader) async throws -> Reader,
         retire: @escaping @MainActor (Reader) -> Void,
         failureMessage: @escaping @MainActor (Error) -> String = { _ in
             "The source could not be pinned. Existing readers are unchanged."
         }) {
        self.canFork = canFork
        self.fork = fork
        self.retire = retire
        self.failureMessage = failureMessage
    }
    deinit { task?.cancel() }

    var selected: Entry? { entries.first { $0.id == selectedID } }
    var canAdd: Bool { root != nil && !busy && entries.count < Self.maximumPins }

    /// Re-enumerating one root does not replace pinned captures. A different
    /// native path is a different authority even if Swift considers the two
    /// Unicode strings canonically equivalent. Grant replacement calls closeAll.
    func bind(root next: String) {
        guard !next.isEmpty, next.utf8.count <= 16_384, !next.utf8.contains(0) else {
            closeAll()
            root = nil
            return
        }
        if root?.utf8.elementsEqual(next.utf8) == true { return }
        closeAll()
        root = next
    }

    func pin(_ reader: Reader, root expectedRoot: String, label: String) {
        guard root?.utf8.elementsEqual(expectedRoot.utf8) == true else { return }
        guard canAdd else {
            notice = busy ? "A source is already being pinned."
                : "Two sources are pinned. Close a pin before adding another."
            return
        }
        guard canFork(reader) else {
            notice = "Wait for the retained reader to finish, then pin its source."
            return
        }
        let ticket = UUID()
        operation = ticket
        busy = true
        notice = "Pinning the retained source…"
        let fork = fork, retire = retire
        task = Task { [weak self] in
            do {
                let child = try await fork(reader)
                guard let self, self.operation == ticket, !Task.isCancelled,
                      self.root?.utf8.elementsEqual(expectedRoot.utf8) == true,
                      self.entries.count < Self.maximumPins else {
                    retire(child)
                    return
                }
                let entry = Entry(id: UUID(), label: label, reader: child)
                self.entries.append(entry)
                self.selectedID = entry.id
                self.busy = false
                self.task = nil
                self.notice = "Pinned source stays available while you open other files."
            } catch {
                guard let self, self.operation == ticket, !Task.isCancelled else { return }
                self.busy = false
                self.task = nil
                self.notice = self.failureMessage(error)
            }
        }
    }

    func select(_ id: UUID) {
        guard entries.contains(where: { $0.id == id }) else { return }
        selectedID = id
    }

    func close(_ id: UUID) {
        guard let index = entries.firstIndex(where: { $0.id == id }) else { return }
        let entry = entries.remove(at: index)
        if selectedID == id { selectedID = entries.last?.id }
        retire(entry.reader)
        notice = ""
    }

    func cancelPin() {
        operation = UUID()
        task?.cancel()
        task = nil
        busy = false
        notice = "Pinning canceled. Existing readers are unchanged."
    }

    func closeAll() {
        cancelPin()
        let previous = entries
        entries = []
        selectedID = nil
        for entry in previous { retire(entry.reader) }
        notice = ""
    }
}
