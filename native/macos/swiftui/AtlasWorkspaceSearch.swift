import Foundation

/// Main-actor choice of source freshness, not another search executor. Every
/// selected operation is submitted to the existing bounded search coordinator.
/// Refresh replaces a capture owner rather than changing bytes under readers.
@MainActor final class AtlasWorkspaceSearch {
    typealias Work = @Sendable (String, String, AtlasSearchCancellation, AtlasSearchAccessLease,
                               @Sendable (AtlasSearchReport) -> Void) throws -> AtlasSearchReport
    typealias ScopedWork = @Sendable (AtlasSearchInput, AtlasSearchCancellation, AtlasSearchAccessLease,
                                     @Sendable (AtlasSearchReport) -> Void) throws -> AtlasSearchReport
    struct Choice: Sendable {
        fileprivate let revision: UUID
        let indexed: Bool
        let work: Work
        let scopedWork: ScopedWork
    }
    private let live: ScopedWork
    private let makeIndex: @MainActor () -> AtlasIndexedSearch
    private var index: AtlasIndexedSearch?
    private var indexed = false
    private var revision = UUID()

    init(live: @escaping Work, makeIndex: @escaping @MainActor () -> AtlasIndexedSearch) {
        self.live = { input, flag, access, publish in
            try input.requireLegacyScope()
            return try live(input.root, input.query, flag, access, publish)
        }
        self.makeIndex = makeIndex
    }
    init(scopedLive: @escaping ScopedWork, makeIndex: @escaping @MainActor () -> AtlasIndexedSearch) {
        self.live = scopedLive
        self.makeIndex = makeIndex
    }

    /// No discovery or source work occurs here. Leaving captured mode releases
    /// its optional cache; an active request or selected reader can still pin it.
    func configure(indexed: Bool) {
        guard self.indexed != indexed else { return }
        self.indexed = indexed
        refresh()
    }
    func refresh() {
        revision = UUID()
        index = nil
    }

    /// Freeze this request's worker choice. Mode changes cannot reroute an
    /// already admitted query midway through capture or verification.
    func selection(indexed: Bool) -> Choice {
        configure(indexed: indexed)
        let scopedWork: ScopedWork
        if indexed {
            let worker = index ?? makeIndex()
            index = worker
            scopedWork = worker.runScoped
        } else { scopedWork = live }
        let work: Work = { root, query, flag, access, publish in
            try scopedWork(AtlasSearchInput(root: root, query: query), flag, access, publish)
        }
        return Choice(revision: revision, indexed: indexed, work: work, scopedWork: scopedWork)
    }
    func accepts(_ choice: Choice) -> Bool {
        choice.revision == revision && choice.indexed == indexed
    }
}
