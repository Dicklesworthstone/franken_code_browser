import Foundation
import Observation

/// Per-panel native state over the retained Rust path index. Content search,
/// atlas rendering and source readers keep their own independent state/owners.
@Observable @MainActor final class AtlasQuickOpenModel {
    var text = "" { didSet { if !oldValue.utf8.elementsEqual(text.utf8) { submit(debounce: true) } } }
    var mode: AtlasFileFindMode = .fuzzy { didSet { if oldValue != mode { submit(debounce: true) } } }
    var matchCase = false { didSet { if oldValue != matchCase { submit(debounce: true) } } }
    var selected: AtlasFileRowID? {
        didSet { if opening && oldValue != selected { cancel() } }
    }
    private(set) var report: AtlasFileFinderReport?
    private(set) var busy = false
    private(set) var opening = false
    private(set) var notice = "Enter a filename or path. File finding does not read source contents."
    @ObservationIgnored private let transport: AtlasFileFinderTransport
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private var finder: AtlasFileFinder?
    @ObservationIgnored private var root = ""
    @ObservationIgnored private var access: AnyObject?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var flag: AtlasSearchCancellation?
    @ObservationIgnored private var operation = UUID()

    init(transport: AtlasFileFinderTransport) { self.transport = transport }
    deinit { task?.cancel(); flag?.cancel() }

    var isCurrent: Bool {
        guard let report, let finder, let query = try? input() else { return false }
        return report.catalog == finder.catalogID && report.query == query && report.root.utf8.elementsEqual(root.utf8)
    }
    var canOpen: Bool {
        !busy && isCurrent && selected.flatMap { report?.row($0)?.path } != nil
    }
    func bind(root: String, accessLease: AnyObject?) {
        close()
        self.root = root; self.access = accessLease
        rebuild()
    }
    func refresh() {
        invalidate(clear: true)
        finder = nil
        rebuild()
    }
    private func rebuild() {
        do {
            finder = try AtlasFileFinder(root: root, accessLease: access, transport: transport)
            notice = "Enter a filename or path. This catalog is refreshed explicitly, not on every keystroke."
            if !text.isEmpty { submit() }
        } catch { notice = Self.message(error) }
    }
    func submit(debounce: Bool = false) {
        // An edit revokes old rows synchronously, even if the new input is invalid.
        invalidate(clear: true)
        guard !text.isEmpty else {
            notice = "Enter a filename or path. File finding does not read source contents."
            return
        }
        let query: AtlasFileQuery
        do { query = try input() }
        catch { notice = Self.message(error); return }
        guard let finder else { notice = "Choose a project before finding files."; return }
        let ticket = operation, flag = AtlasSearchCancellation(), io = io
        self.flag = flag; busy = true
        notice = "Finding files in the frozen catalog…"
        task = Task { [weak self] in
            do {
                if debounce { try await Task.sleep(nanoseconds: 150_000_000) }
                try Task.checkCancellation()
                let result = try await io.perform(cancellation: flag, keepingAlive: [finder]) {
                    try finder.find(query, cancellation: flag)
                }
                guard let self, self.operation == ticket, !Task.isCancelled, !flag.isCanceled,
                      self.finder === finder, (try? self.input()) == query else { return }
                self.report = result
                self.finish()
                self.selected = result.rows.first(where: { $0.path != nil }).map { result.rowID($0.id) }
                self.notice = result.summary
            } catch {
                guard let self, self.operation == ticket, !Task.isCancelled else { return }
                self.finish(); self.notice = Self.message(error)
            }
        }
    }
    func move(backwards: Bool) {
        guard !busy, isCurrent, let report else { return }
        let rows = report.rows.filter { $0.path != nil }
        guard !rows.isEmpty else { return }
        let current = selected.flatMap { id in rows.firstIndex { $0.id == id.file && id.report == report.id } }
        let index: Int
        if let current { index = backwards ? (current + rows.count - 1) % rows.count : (current + 1) % rows.count }
        else { index = backwards ? rows.count - 1 : 0 }
        selected = report.rowID(rows[index].id)
    }
    func activate(deliver: @escaping @MainActor (AtlasFileOpenChoice) -> Void) {
        if canOpen { openSelected(deliver: deliver) }
        else if !opening { submit() }
    }
    func openSelected(deliver: @escaping @MainActor (AtlasFileOpenChoice) -> Void) {
        guard canOpen, let finder, let report, let selected else { return }
        invalidate(clear: false)
        let ticket = operation, flag = AtlasSearchCancellation(), io = io
        self.flag = flag; busy = true; opening = true
        notice = "Verifying the selected file identity…"
        task = Task { [weak self] in
            do {
                let choice = try await io.perform(cancellation: flag, keepingAlive: [finder]) {
                    try finder.select(selected, cancellation: flag)
                }
                guard let self, self.operation == ticket, !Task.isCancelled, !flag.isCanceled,
                      self.finder === finder, self.isCurrent, self.report?.id == report.id,
                      self.selected == selected, choice.catalog == report.catalog,
                      choice.report == report.id, choice.root.utf8.elementsEqual(self.root.utf8) else { return }
                self.finish()
                // The app rechecks its project context and chooses its ordinary
                // source opener. Metadata selection never supplies exact-hit offsets.
                deliver(choice)
            } catch {
                guard let self, self.operation == ticket, !Task.isCancelled else { return }
                self.finish(); self.notice = Self.message(error)
            }
        }
    }
    func cancel() {
        invalidate(clear: false)
        notice = "File finding canceled. No new file was opened."
    }
    func close() {
        invalidate(clear: true)
        finder = nil; access = nil; root = ""
    }
    private func input() throws -> AtlasFileQuery { try .init(text: text, mode: mode, matchCase: matchCase) }
    private func invalidate(clear: Bool) {
        operation = UUID()
        task?.cancel(); task = nil; flag?.cancel(); flag = nil
        io.cancel(); busy = false; opening = false
        if clear { report = nil; selected = nil }
    }
    private func finish() { busy = false; opening = false; task = nil; flag = nil }
    private static func message(_ error: Error) -> String {
        if let error = error as? AtlasFileFinderError { return error.message }
        if error is CancellationError { return AtlasFileFinderError.canceled.message }
        if let error = error as? AtlasProjectIOError, case .canceled = error { return AtlasFileFinderError.canceled.message }
        return AtlasFileFinderError.unavailable.message
    }
}
