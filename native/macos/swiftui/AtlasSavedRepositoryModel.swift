import Foundation
import Observation
import Dispatch

private final class AtlasSavedIndexAccess: @unchecked Sendable {
    let generation: UInt64
    let pin, path: String
    let reference: AnyObject?
    init(generation: UInt64, pin: String, path: String, reference: AnyObject?) {
        self.generation = generation; self.pin = pin; self.path = path; self.reference = reference
    }
    func matches(_ index: AtlasSavedIndex) -> Bool { generation == index.generation && pin == index.pin }
}

/// At most one accepted and one pending index grant accompany the archive.
/// A missing reply never releases either grant until info identifies the
/// accepted generation, or the archive descriptor drains during retirement.
private final class AtlasSavedAccess: @unchecked Sendable {
    let reference: AnyObject?
    private let lock = NSLock()
    private static let retirement = DispatchQueue(label: "dev.frankencode.browser.saved-index-access-retirement", qos: .utility)
    private var index: AtlasSavedIndexAccess?
    private var candidate: AtlasSavedIndexAccess?
    init(_ reference: AnyObject?) { self.reference = reference }
    func stage(_ access: AtlasSavedIndexAccess) {
        lock.lock(); defer { lock.unlock() }
        precondition(candidate == nil)
        candidate = access
    }
    func resolve(_ accepted: AtlasSavedIndex?) throws -> String? {
        lock.lock(); defer { lock.unlock() }
        guard let accepted else {
            let retired = (index, candidate); index = nil; candidate = nil
            Self.retirement.async { withExtendedLifetime(retired) {} }
            return nil
        }
        if let candidate, candidate.matches(accepted) {
            let retired = index; index = candidate; self.candidate = nil
            Self.retirement.async { withExtendedLifetime(retired) {} }
        } else if index?.matches(accepted) == true {
            let retired = candidate; candidate = nil
            Self.retirement.async { withExtendedLifetime(retired) {} }
        } else { throw AtlasSavedError.invalidResponse }
        return index?.path
    }
}
/// Revocation is independent of ARC. An already entered worker may retain the
/// archive lease while it drains, but no later image operation may acquire it.
private final class AtlasSavedImageRevocation: @unchecked Sendable {
    private let lock = NSLock()
    private var active = true
    var isActive: Bool { lock.lock(); defer { lock.unlock() }; return active }
    @discardableResult func revoke() -> Bool {
        lock.lock(); defer { lock.unlock() }
        let wasActive = active; active = false; return wasActive
    }
}
private final class AtlasSavedSession: @unchecked Sendable {
    let handle: UInt64
    let transport: AtlasSavedTransport
    private let access: AtlasSavedAccess
    private let images = AtlasSavedImageRevocation()
    private static let retirement = DispatchQueue(label: "dev.frankencode.browser.saved-retirement", qos: .utility)
    init(transport: AtlasSavedTransport, access: AnyObject?) throws {
        let handle = transport.create()
        guard handle != 0 else { throw AtlasSavedError.unavailable }
        self.handle = handle; self.transport = transport; self.access = AtlasSavedAccess(access)
    }
    func cancel() { _ = transport.cancel(handle) }
    func stageIndex(_ candidate: AtlasSavedIndexAccess) { access.stage(candidate) }
    func resolveIndex(_ accepted: AtlasSavedIndex?) throws -> String? { try access.resolve(accepted) }
    func revokeDocumentImages() {
        if images.revoke() { cancel() }
    }
    func documentTransport(_ document: AtlasReaderDocumentTransport?, archive: AtlasSavedIdentity,
        sourceMember: UInt64) throws -> AtlasReaderDocumentTransport? {
        guard var document else { return nil }
        // Never inherit another archive's provider when following a saved link.
        document.savedImages = nil
        guard let authorize = transport.imagesAuthorize, let prepare = transport.imagePrepare else { return document }
        guard archive.owner == handle else { throw AtlasSavedError.invalidResponse }
        let source = try AtlasSavedDocumentImageSource(archiveOwner: handle, archiveDigest: archive.digest,
            sourceMember: sourceMember, members: archive.members)
        let gate = images
        document.savedImages = .init(source: source,
            isAvailable: { [weak self] in gate.isActive && self != nil },
            authorize: { [weak self] reader, generation in
                guard gate.isActive, let lease = self else { return nil }
                return withExtendedLifetime(lease) {
                    let result = authorize(lease.handle, reader, sourceMember, generation)
                    return gate.isActive ? result : nil
                }
            },
            prepare: { [weak self] reader, generation, item, request in
                guard gate.isActive, let lease = self else { return nil }
                return withExtendedLifetime(lease) {
                    let result = prepare(lease.handle, reader, sourceMember, generation, item, request)
                    return gate.isActive ? result : nil
                }
            })
        return document
    }
    deinit {
        images.revoke()
        let handle = handle, transport = transport, access = access
        Self.retirement.async {
            withExtendedLifetime(access) {
                for attempt in 0..<8 {
                    if transport.close(handle) { return }
                    if attempt < 7 { Thread.sleep(forTimeInterval: 0.001) }
                }
                transport.retirementFailed()
            }
        }
    }
}

/// One accepted archive, one pending replacement and one worker operation.
/// Member/link pagination owns bounded metadata; opened readers independently
/// own verified source bytes. The archive pathname is never used after opening.
@Observable @MainActor final class AtlasSavedRepositoryModel {
    private(set) var info: AtlasSavedInfo?
    private(set) var members: AtlasSavedMembers?
    private(set) var selectedMember: AtlasSavedMember?
    private(set) var relationships: AtlasSavedRelationships?
    private(set) var direction: AtlasSavedDirection = .all
    private(set) var relationshipGeneration: UInt64?
    private(set) var imports: AtlasSavedImports?
    private(set) var importDirection: AtlasSavedDirection = .all
    private(set) var importGeneration: UInt64?
    private(set) var pathPage: AtlasSavedPaths?
    private(set) var pathGeneration: UInt64?
    private(set) var searchPage: AtlasSavedSearchPage?
    private(set) var queryGeneration: UInt64?
    private(set) var expressionPage: AtlasSavedExpressionPage?
    private(set) var expressionGeneration: UInt64?
    private(set) var structurePage: AtlasSavedStructurePage?
    private(set) var structureGeneration: UInt64?
    private(set) var indexStateKnown = true
    private(set) var indexPath: String?
    private(set) var busy = false
    private(set) var closed = false
    private(set) var notice = "Open an FCBS saved repository to browse its captured members and documentation links."
    var memberOffsetInput = "0"
    var pathInput = ""
    var pathMode: AtlasSavedPathMode = .fuzzy
    var pathCaseSensitive = false
    var searchInput = ""
    var searchMode: AtlasSavedSearchMode = .literal
    var searchNameMode: AtlasSearchNameMode = .contains
    var countAllMatches = true
    var searchCaseSensitive = true
    var indexPinInput = ""
    @ObservationIgnored private let transport: AtlasSavedTransport
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private var session: AtlasSavedSession?
    @ObservationIgnored private var pending: AtlasSavedSession?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var cancellation: AtlasSearchCancellation?
    @ObservationIgnored private var lastRelationshipAttempt: UInt64 = 0
    @ObservationIgnored private var lastImportAttempt: UInt64 = 0
    @ObservationIgnored private var lastPathAttempt: UInt64 = 0
    @ObservationIgnored private var lastQueryAttempt: UInt64 = 0
    @ObservationIgnored private var lastExpressionAttempt: UInt64 = 0
    @ObservationIgnored private var lastStructureAttempt: UInt64 = 0
    @ObservationIgnored private var lastIndexAttempt: UInt64 = 0

    init(transport: AtlasSavedTransport) { self.transport = transport }
    var canOperate: Bool { !busy && !closed }
    var readerDomain: UUID? { transport.readerDomain }
    var validPathInput: Bool { AtlasSavedPathWire.validQuery(Array(pathInput.utf8)) }
    var pathSearchIsCurrent: Bool {
        guard let pathPage, info?.identity == pathPage.identity, pathGeneration == pathPage.generation else { return false }
        return Array(pathInput.utf8) == pathPage.query && pathMode == pathPage.mode && pathCaseSensitive == pathPage.caseSensitive
    }
    var importsAreCurrent: Bool {
        guard let imports, info?.identity == imports.identity, importGeneration == imports.generation,
              importDirection == imports.direction else { return false }
        return imports.direction == .all || imports.member == selectedMember?.id
    }
    var validSearchInput: Bool { AtlasSavedSearchWire.validNeedle(searchInput) }
    var index: AtlasSavedIndex? { info?.index }
    var canChangeIndex: Bool { canOperate && info != nil && indexStateKnown }
    var canSearch: Bool { canOperate && (searchMode != .literal || indexStateKnown) }
    var searchIsCurrent: Bool {
        guard searchMode == .literal, let searchPage, info?.identity == searchPage.identity,
              queryGeneration == searchPage.generation else { return false }
        return searchInput.utf8.elementsEqual(searchPage.needle.utf8) && countAllMatches == searchPage.counting.all
            && searchCaseSensitive == searchPage.caseSensitive
    }
    var expressionIsCurrent: Bool {
        guard searchMode == .expression, let expressionPage, info?.identity == expressionPage.identity,
              expressionGeneration == expressionPage.generation else { return false }
        return searchInput.utf8.elementsEqual(expressionPage.expression.utf8) && countAllMatches == expressionPage.counting.all
    }

    var structureIsCurrent: Bool {
        guard let structurePage, searchMode == structurePage.mode,
              searchMode.effectiveNameMode(searchNameMode) == structurePage.nameMode,
              info?.identity == structurePage.identity, structureGeneration == structurePage.generation else { return false }
        return searchInput.utf8.elementsEqual(structurePage.needle.utf8)
    }

    func open(path: String, accessLease: AnyObject?, expectedDigest: String? = nil) {
        guard canOperate, path.hasPrefix("/"), path.utf8.count <= 16_384, !path.utf8.contains(0),
              expectedDigest.map({ AtlasSavedWire.digest($0) }) != false else { return }
        perform { model, flag in
            let candidate = try AtlasSavedSession(transport: model.transport, access: accessLease)
            model.pending = candidate
            model.notice = "Validating the selected saved archive…"
            do {
                let json = try await model.run(candidate, .open(path: path), flag: flag)
                let opened: AtlasSavedInfo
                do { opened = try AtlasSavedInfo.decode(json, handle: candidate.handle, expectedPath: Array(path.utf8), fresh: true) }
                catch {
                    // An initial open can accept before its reply is lost. Read
                    // info once on that same handle; never replay a pathname open.
                    guard !flag.isCanceled, !model.closed else { throw AtlasSavedError.canceled }
                    opened = try AtlasSavedInfo.decode(await model.run(candidate, .info, flag: flag),
                        handle: candidate.handle, expectedPath: Array(path.utf8), fresh: true)
                }
                if let expectedDigest, opened.identity.digest != expectedDigest {
                    // The saved pathname may now name a different valid archive.
                    // Keep the previous browser and never publish unrelated rows
                    // as the result of this exact export receipt.
                    if model.pending === candidate { model.pending = nil; candidate.cancel() }
                    model.notice = "The selected destination now contains a different archive from the saved result. The previously opened repository is unchanged. Choose Open archive to inspect it explicitly."
                    return
                }
                let page = try AtlasSavedMembers.decode(await model.run(candidate, .members(start: 0, limit: 64), flag: flag),
                    identity: opened.identity, start: 0, limit: 64)
                guard !flag.isCanceled, !model.closed, model.pending === candidate else { throw AtlasSavedError.canceled }
                model.session?.revokeDocumentImages()
                model.session = candidate; model.pending = nil
                model.info = opened; model.members = page; model.selectedMember = page.items.first
                model.relationships = nil; model.relationshipGeneration = nil; model.direction = .all
                model.imports = nil; model.importGeneration = nil; model.importDirection = .all; model.lastImportAttempt = 0
                model.pathPage = nil; model.pathGeneration = nil; model.lastPathAttempt = 0; model.pathInput = ""
                model.lastRelationshipAttempt = 0; model.memberOffsetInput = "0"
                model.searchPage = nil; model.queryGeneration = nil; model.lastQueryAttempt = 0; model.searchInput = ""
                model.expressionPage = nil; model.expressionGeneration = nil; model.lastExpressionAttempt = 0
                model.structurePage = nil; model.structureGeneration = nil; model.lastStructureAttempt = 0
                model.indexStateKnown = true; model.indexPath = nil; model.lastIndexAttempt = 0; model.indexPinInput = ""
                model.notice = "Opened the saved manifest. Search captured text, or build documentation links and source imports when ready."
            } catch {
                if model.pending === candidate { model.pending = nil; candidate.cancel() }
                throw error
            }
        }
    }

    func attachIndex(path: String, pin: String, accessLease: AnyObject?) {
        guard canChangeIndex, let info else { return }
        guard path.hasPrefix("/"), path.utf8.count <= 16_384, !path.utf8.contains(0), AtlasSavedWire.digest(pin) else {
            notice = "Choose an FCBD index and enter the 64 lowercase hexadecimal characters from its trusted build receipt."; return
        }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextIndexGeneration()
            lease.stageIndex(AtlasSavedIndexAccess(generation: generation, pin: pin, path: path, reference: accessLease))
            model.notice = "Checking the selected index against the saved archive and trusted page-manifest pin…"
            do {
                let index = try AtlasSavedIndex.decodeAttach(await model.run(lease,
                    .attachIndex(generation: generation, path: path, pin: pin), flag: flag),
                    identity: info.identity, generation: generation, pin: pin)
                guard model.session === lease, !model.closed else { throw AtlasSavedError.canceled }
                try model.acceptIndex(index, attempt: generation, lease: lease)
                model.notice = "Attached the saved index. New literal searches use verified posting pages and exact captured-source checks."
            } catch {
                let failure = Self.message(error)
                let recovered = await model.reconcileIndex(lease)
                model.notice = recovered ? "\(failure) Recovered the accepted index without reopening its path."
                    : "\(failure) Index acceptance is unconfirmed. Reload index status before attaching another index or starting a literal search."
            }
        }
    }
    func detachIndex() {
        guard canChangeIndex, let info, info.index != nil else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextIndexGeneration()
            do {
                try AtlasSavedIndex.decodeDetach(await model.run(lease, .detachIndex(generation: generation), flag: flag),
                    identity: info.identity, generation: generation)
                guard model.session === lease, !model.closed else { throw AtlasSavedError.canceled }
                try model.acceptIndex(nil, attempt: generation, lease: lease)
                model.notice = "Detached the index. New literal searches scan captured source directly; accepted results remain available."
            } catch {
                let failure = Self.message(error)
                let recovered = await model.reconcileIndex(lease)
                model.notice = recovered ? "\(failure) Recovered the accepted index after the detach request."
                    : "\(failure) Detach is unconfirmed. Reload index status to inspect the accepted state."
            }
        }
    }
    func reloadIndex() {
        guard info != nil else { return }
        perform { model, _ in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let recovered = await model.reconcileIndex(lease)
            model.notice = recovered ? "Reloaded index coverage and page I/O without reopening source or index paths."
                : "Index acceptance is unconfirmed. Its file grants remain held until status is recovered or the archive is closed."
        }
    }
    private func acceptIndex(_ index: AtlasSavedIndex?, attempt: UInt64, lease: AtlasSavedSession) throws {
        indexPath = try lease.resolveIndex(index)
        self.info?.index = index; self.info?.lastIndexAttempt = attempt
        lastIndexAttempt = max(lastIndexAttempt, attempt); indexStateKnown = true
    }
    private func acceptInfo(_ current: AtlasSavedInfo, lease: AtlasSavedSession) throws {
        guard current.lastIndexAttempt >= (info?.lastIndexAttempt ?? 0) else { throw AtlasSavedError.invalidResponse }
        indexPath = try lease.resolveIndex(current.index)
        info = current; lastIndexAttempt = max(lastIndexAttempt, current.lastIndexAttempt); indexStateKnown = true
    }
    private func reconcileIndex(_ lease: AtlasSavedSession) async -> Bool {
        guard !closed, session === lease, let previous = info else { return false }
        do {
            let current = try AtlasSavedInfo.decode(await run(lease, .info, flag: AtlasSearchCancellation()),
                handle: lease.handle, expectedPath: previous.path.bytes, identity: previous.identity)
            guard !closed, session === lease else { return false }
            try acceptInfo(current, lease: lease); return true
        } catch {
            indexStateKnown = false
            return false
        }
    }

    func loadMembers(start: UInt64) {
        guard let info, start <= info.identity.members else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let page = try AtlasSavedMembers.decode(await model.run(lease, .members(start: start, limit: 64), flag: flag),
                identity: info.identity, start: start, limit: 64)
            guard model.session === lease, !model.closed else { throw AtlasSavedError.canceled }
            model.members = page; model.memberOffsetInput = String(start)
            model.notice = "Showing saved member metadata \(start)..<\(start + UInt64(page.items.count)). No member payloads were read."
        }
    }
    func goToMemberPage() {
        guard let start = AtlasReaderWire.integer(memberOffsetInput), let info, start <= info.identity.members else {
            notice = "Enter a member ordinal from 0 through \(info?.identity.members ?? 0)."; return
        }
        loadMembers(start: start)
    }
    func selectMember(_ member: AtlasSavedMember, from identity: AtlasSavedIdentity, forImports: Bool = false) {
        guard canOperate, info?.identity == identity, members?.identity == identity,
              members?.items.contains(where: { $0.matches(member) }) == true else { return }
        selectAcceptedMember(member, forImports: forImports)
    }
    private func selectAcceptedMember(_ member: AtlasSavedMember, forImports: Bool) {
        selectedMember = member
        if direction != .all { relationships = nil }
        if importDirection != .all { imports = nil }
        if forImports, importDirection != .all, importGeneration != nil {
            loadImports(direction: importDirection)
        } else if !forImports, direction != .all, relationshipGeneration != nil {
            loadRelationships(direction: direction)
        }
    }

    func searchPaths() {
        guard canOperate, info != nil else { return }
        if pathInput.isEmpty { clearPaths(); return }
        guard validPathInput else { notice = "Enter a filename or path using up to 256 UTF-8 bytes, without NUL."; return }
        submitPaths(query: Array(pathInput.utf8), mode: pathMode, caseSensitive: pathCaseSensitive, work: AtlasSavedPathWire.defaultWork)
    }
    func searchMorePaths() {
        guard pathSearchIsCurrent, let page = pathPage, page.canSearchMore else { return }
        submitPaths(query: page.query, mode: page.mode, caseSensitive: page.caseSensitive, work: page.expandedWork)
    }
    private func submitPaths(query: [UInt8], mode: AtlasSavedPathMode, caseSensitive: Bool, work: UInt64) {
        guard let info else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextPathGeneration()
            model.notice = "Finding filenames across the saved manifest…"
            do {
                let page = try AtlasSavedPaths.decode(await model.run(lease,
                    .pathSearch(generation: generation, query: query, mode: mode, caseSensitive: caseSensitive,
                        results: AtlasSavedPathWire.maximumResults, work: work), flag: flag), identity: info.identity,
                    generation: generation, query: query, mode: mode, caseSensitive: caseSensitive,
                    maxResults: AtlasSavedPathWire.maximumResults, maxWorkUnits: work, start: 0, limit: 64)
                guard !model.closed, !flag.isCanceled, model.session === lease else { throw AtlasSavedError.canceled }
                model.pathGeneration = generation; model.pathPage = page
                model.notice = page.complete ? "Searched filenames throughout the saved manifest without reading source payloads."
                    : "Filename results are partial. Search more to inspect additional candidates, or narrow the query."
            } catch {
                let recovered = await model.reconcilePaths(lease)
                model.notice = recovered ? "Recovered the accepted filename results without repeating the search."
                    : "The accepted filename results could not be verified. Reload names before opening a result."
            }
        }
    }
    func loadPaths(start: UInt64) {
        guard pathSearchIsCurrent, let page = pathPage else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let next = try AtlasSavedPaths.decode(await model.run(lease,
                .pathResults(generation: page.generation, start: start, limit: 64), flag: flag), identity: page.identity,
                generation: page.generation, query: page.query, mode: page.mode, caseSensitive: page.caseSensitive,
                maxResults: page.maxResults, maxWorkUnits: page.maxWorkUnits, start: start, limit: 64)
            guard !model.closed, !flag.isCanceled, model.session === lease, model.pathGeneration == page.generation,
                  next.matchesInventory(page) else { throw AtlasSavedError.canceled }
            model.pathPage = next
            model.notice = "Showing retained filename results. Paging does not repeat the search or load source payloads."
        }
    }
    func reloadPaths() {
        perform { model, _ in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let recovered = await model.reconcilePaths(lease)
            model.notice = recovered ? "Reloaded accepted filename results from the retained saved query."
                : "Filename results are unavailable. Search names again when ready."
        }
    }
    func clearPaths() {
        guard let info else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextPathGeneration()
            do {
                try AtlasSavedPathWire.clear(await model.run(lease, .clearPaths(generation: generation), flag: flag),
                    identity: info.identity, generation: generation)
                guard !model.closed, !flag.isCanceled, model.session === lease else { throw AtlasSavedError.canceled }
                model.pathGeneration = nil; model.pathPage = nil
                model.notice = "Cleared filename results. Source search and relationship inventories are unchanged."
            } catch {
                let recovered = await model.reconcilePaths(lease)
                model.notice = recovered ? "Recovered the accepted filename state after the clear reply was unavailable."
                    : "Filename state is unknown. Reload names before continuing."
            }
        }
    }
    private func reconcilePaths(_ lease: AtlasSavedSession) async -> Bool {
        guard !closed, session === lease, let previous = info else { return false }
        let flag = AtlasSearchCancellation()
        do {
            let current = try AtlasSavedInfo.decode(await run(lease, .info, flag: flag), handle: lease.handle,
                expectedPath: previous.path.bytes, identity: previous.identity)
            guard !closed, session === lease, current.pathStateKnown,
                  current.lastPathAttempt >= previous.lastPathAttempt else { throw AtlasSavedError.invalidResponse }
            try acceptInfo(current, lease: lease); lastPathAttempt = max(lastPathAttempt, current.lastPathAttempt)
            pathGeneration = current.pathGeneration
            guard let generation = current.pathGeneration else { pathPage = nil; return true }
            if pathPage?.generation == generation { return true }
            let page = try AtlasSavedPaths.decode(await run(lease, .pathResults(generation: generation, start: 0, limit: 64), flag: flag),
                identity: current.identity, generation: generation, start: 0, limit: 64)
            guard !closed, session === lease else { return false }
            pathPage = page; return true
        } catch { pathGeneration = nil; pathPage = nil; return false }
    }
    private func ownsPathHit(_ hit: AtlasSavedPathHit, from page: AtlasSavedPaths) -> Bool {
        guard pathSearchIsCurrent, let current = pathPage, current.matchesInventory(page), current.start == page.start else { return false }
        return current.hits.contains { $0.matches(hit) } && page.hits.contains { $0.matches(hit) }
    }
    func selectPathHit(_ hit: AtlasSavedPathHit, from page: AtlasSavedPaths, forImports: Bool = false) {
        guard canOperate, ownsPathHit(hit, from: page) else { return }
        selectAcceptedMember(hit.member, forImports: forImports)
    }
    func openPathHit(_ hit: AtlasSavedPathHit, from page: AtlasSavedPaths, readerTransport: AtlasReaderTransport,
        search: AtlasReaderSearchTransport? = nil, outline: AtlasReaderOutlineTransport? = nil,
        document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasSavedReaderCapture {
        guard ownsPathHit(hit, from: page), hit.member.captured else { throw AtlasSavedError.invalidInput }
        return try await openReader(saved: page.identity, ordinal: hit.member.id, expected: hit.member,
            origin: .pathHit(generation: page.generation, hit: hit), readerTransport: readerTransport,
            search: search, outline: outline, document: document)
    }

    func prepareImports() {
        guard let info else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextImportGeneration()
            model.notice = "Parsing captured Rust, JavaScript, TypeScript and Python import statements…"
            do {
                let page = try AtlasSavedImports.decode(await model.run(lease,
                    .prepareImports(generation: generation, imports: AtlasSavedImportWire.maximumLinks,
                        bytes: AtlasSavedImportWire.maximumSourceBytes), flag: flag), identity: info.identity,
                    generation: generation, direction: .all, member: nil, start: 0, limit: 64, preparing: true)
                guard model.session === lease, !model.closed, !flag.isCanceled else { throw AtlasSavedError.canceled }
                model.importGeneration = generation; model.imports = page; model.importDirection = .all
                model.notice = page.complete ? "Lexical imports are complete within this saved manifest; targets are catalog candidates."
                    : "Source import coverage is partial. Missing incoming imports cannot be ruled out; inspect the omission counts."
            } catch {
                let restored = await model.reconcileImports(lease)
                model.notice = restored
                    ? "The build reply was not accepted. Recovered the accepted import inventory without rebuilding."
                    : "The import generation could not be verified. Reload imports before navigating results."
            }
        }
    }
    func loadImports(direction: AtlasSavedDirection, start: UInt64 = 0) {
        guard let info, let generation = importGeneration else { return }
        let member = direction == .all ? nil : selectedMember?.id
        guard direction == .all || member != nil else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let page = try AtlasSavedImports.decode(await model.run(lease,
                .imports(generation: generation, direction: direction, member: member, start: start, limit: 64), flag: flag),
                identity: info.identity, generation: generation, direction: direction, member: member, start: start, limit: 64)
            guard model.session === lease, !model.closed, !flag.isCanceled,
                  model.importGeneration == generation else { throw AtlasSavedError.canceled }
            model.imports = page; model.importDirection = direction
            model.notice = "Showing \(page.links.count) of \(page.matching) matching imports. Pagination reads retained metadata only."
        }
    }
    func reloadImports() {
        perform { model, _ in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let recovered = await model.reconcileImports(lease)
            model.notice = recovered ? "Reloaded the accepted import generation without parsing source again."
                : "The accepted import generation is unavailable. Saved members remain readable independently."
        }
    }
    func clearImports() {
        guard let info, importGeneration != nil else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextImportGeneration()
            do {
                try AtlasSavedImportWire.clear(await model.run(lease, .clearImports(generation: generation), flag: flag),
                    identity: info.identity, generation: generation)
                guard model.session === lease, !model.closed, !flag.isCanceled else { throw AtlasSavedError.canceled }
                model.imports = nil; model.importGeneration = nil; model.importDirection = .all
                model.notice = "Cleared retained import metadata. Open source readers keep their captured bytes."
            } catch {
                let recovered = await model.reconcileImports(lease)
                model.notice = recovered ? "Recovered the accepted import state after the clear reply was unavailable."
                    : "The accepted import state is unknown. Reload imports before continuing."
            }
        }
    }
    private func reconcileImports(_ lease: AtlasSavedSession) async -> Bool {
        guard !closed, session === lease, let previous = info else { return false }
        let flag = AtlasSearchCancellation()
        do {
            let current = try AtlasSavedInfo.decode(await run(lease, .info, flag: flag), handle: lease.handle,
                expectedPath: previous.path.bytes, identity: previous.identity)
            guard !closed, session === lease, current.importStateKnown,
                  current.lastImportAttempt >= previous.lastImportAttempt else { throw AtlasSavedError.invalidResponse }
            try acceptInfo(current, lease: lease)
            lastImportAttempt = max(lastImportAttempt, current.lastImportAttempt)
            importGeneration = current.importGeneration
            guard let generation = current.importGeneration else { imports = nil; importDirection = .all; return true }
            if importsAreCurrent { return true }
            let page = try AtlasSavedImports.decode(await run(lease,
                .imports(generation: generation, direction: .all, member: nil, start: 0, limit: 64), flag: flag),
                identity: current.identity, generation: generation, direction: .all, member: nil, start: 0, limit: 64)
            guard !closed, session === lease else { return false }
            imports = page; importDirection = .all; return true
        } catch {
            importGeneration = nil; imports = nil
            return false
        }
    }

    func openImport(_ link: AtlasSavedImport, from page: AtlasSavedImports, side: AtlasSavedLinkSide,
        readerTransport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil,
        outline: AtlasReaderOutlineTransport? = nil, document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasSavedReaderCapture {
        guard importsAreCurrent, let current = imports, current.matchesInventory(page), current.start == page.start,
              current.links.contains(where: { $0.matches(link) }), page.links.contains(where: { $0.matches(link) }),
              side == .evidence || link.canOpenTarget,
              let ordinal = side == .evidence ? link.sourceMember : link.targetMember else { throw AtlasSavedError.invalidInput }
        return try await openReader(saved: page.identity, ordinal: ordinal, expected: nil,
            origin: .importLink(generation: page.generation, link: link, side: side), readerTransport: readerTransport,
            search: search, outline: outline, document: document)
    }

    func prepareRelationships() {
        guard let info else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextRelationshipGeneration()
            model.notice = "Parsing captured Markdown documents for links and backlinks…"
            do {
                let page = try AtlasSavedRelationships.decode(await model.run(lease,
                    .prepareRelationships(generation: generation, links: 4096, bytes: AtlasSavedWire.relationshipBytes), flag: flag),
                    identity: info.identity, generation: generation, direction: .all, member: nil, start: 0, limit: 64, preparing: true)
                guard model.session === lease, !model.closed else { throw AtlasSavedError.canceled }
                model.relationshipGeneration = generation; model.relationships = page; model.direction = .all
                model.notice = page.complete ? "Documentation links are complete for the parsed saved manifest."
                    : "Documentation link coverage is partial. Missing backlinks cannot be ruled out; inspect the omission counts."
            } catch {
                let restored = await model.reconcileRelationships(lease)
                model.notice = restored
                    ? "The build reply was not accepted. The currently accepted link generation has been recovered without rebuilding."
                    : "The link generation could not be verified. Reload links before navigating relationship results."
            }
        }
    }
    func loadRelationships(direction: AtlasSavedDirection, start: UInt64 = 0) {
        guard let info, let generation = relationshipGeneration else { return }
        let member = direction == .all ? nil : selectedMember?.id
        guard direction == .all || member != nil else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let page = try AtlasSavedRelationships.decode(await model.run(lease,
                .relationships(generation: generation, direction: direction, member: member, start: start, limit: 64), flag: flag),
                identity: info.identity, generation: generation, direction: direction, member: member, start: start, limit: 64)
            guard model.session === lease, !model.closed, model.relationshipGeneration == generation else { throw AtlasSavedError.canceled }
            model.relationships = page; model.direction = direction
            model.notice = "Showing \(page.links.count) of \(page.matching) matching saved links. Pagination does not read member payloads."
        }
    }
    func reloadRelationships() {
        perform { model, _ in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let recovered = await model.reconcileRelationships(lease)
            model.notice = recovered ? "Reloaded the accepted link generation without parsing or reopening source paths."
                : "The accepted link generation is unavailable. Saved members remain available independently."
        }
    }
    private func reconcileRelationships(_ lease: AtlasSavedSession) async -> Bool {
        guard !closed, session === lease, let previous = info else { return false }
        let flag = AtlasSearchCancellation()
        do {
            let current = try AtlasSavedInfo.decode(await run(lease, .info, flag: flag),
                handle: lease.handle, expectedPath: previous.path.bytes, identity: previous.identity)
            guard !closed, session === lease else { return false }
            try acceptInfo(current, lease: lease); relationshipGeneration = current.relationshipGeneration
            guard let generation = current.relationshipGeneration else { relationships = nil; direction = .all; return true }
            lastRelationshipAttempt = max(lastRelationshipAttempt, generation)
            if relationships?.generation == generation { return true }
            let page = try AtlasSavedRelationships.decode(await run(lease,
                .relationships(generation: generation, direction: .all, member: nil, start: 0, limit: 64), flag: flag),
                identity: current.identity, generation: generation, direction: .all, member: nil, start: 0, limit: 64)
            guard !closed, session === lease else { return false }
            relationships = page; direction = .all; return true
        } catch {
            relationshipGeneration = nil; relationships = nil
            return false
        }
    }

    /// Explicit submission freezes the exact UTF-8 needle. Editing the draft
    /// never changes the label or navigation authority of an accepted query.
    func search() {
        if searchMode == .expression { searchExpression(); return }
        if searchMode.structure != nil { searchStructure(); return }
        guard canSearch, let info else { return }
        if searchInput.isEmpty { clearSearch(); return }
        guard validSearchInput else {
            notice = "Enter 1–1024 UTF-8 bytes of literal text, without NUL. Matching does not normalize Unicode."; return
        }
        let needle = searchInput, counting = countAllMatches, caseSensitive = searchCaseSensitive
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextQueryGeneration()
            model.notice = "Searching decoded text across the captured archive…"
            do {
                let request: AtlasSavedRequest
                if caseSensitive {
                    request = counting ? .countedSearch(generation: generation, needle: needle, matches: 4096)
                        : .search(generation: generation, needle: needle, matches: 4096)
                } else {
                    request = .matchingSearch(generation: generation, needle: needle,
                        caseSensitive: false, countAllMatches: counting, matches: 4096)
                }
                let page = try AtlasSavedSearchPage.decode(await model.run(lease, request, flag: flag),
                    identity: info.identity, generation: generation, needle: needle, start: 0, limit: 64,
                    searching: true, countAllMatches: counting, caseSensitive: caseSensitive)
                guard page.counting.maximum == 4096,
                      page.indexGeneration == (caseSensitive ? info.index?.generation : nil),
                      page.indexPin == (caseSensitive ? info.index?.pin : nil) else {
                    throw AtlasSavedError.invalidResponse
                }
                guard model.session === lease, !model.closed else { throw AtlasSavedError.canceled }
                model.queryGeneration = generation; model.searchPage = page
                model.notice = page.complete ? "Search finished across the complete captured manifest."
                    : "Search coverage is partial. Inspect unavailable files, unsupported decoding and the retained-match limit."
            } catch {
                let recovered = await model.reconcileSearch(lease)
                model.notice = recovered
                    ? "Recovered the accepted search state after an unconfirmed reply. The archive was not searched a second time."
                    : "The accepted search could not be verified. Reload results before navigating search hits."
            }
        }
    }
    func clearSearch() {
        if searchMode == .expression { clearExpression(); return }
        if searchMode.structure != nil { clearStructure(); return }
        guard canOperate, let info else { return }
        searchInput = ""
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextQueryGeneration()
            do {
                try AtlasSavedSearchPage.decodeClear(await model.run(lease, .clearResults(generation: generation), flag: flag),
                    identity: info.identity)
                guard model.session === lease, !model.closed else { throw AtlasSavedError.canceled }
                model.queryGeneration = nil; model.searchPage = nil
                model.notice = "Cleared saved text-search results. Documentation links and captured readers remain available."
            } catch {
                let recovered = await model.reconcileSearch(lease)
                model.notice = recovered ? "Recovered the accepted search state after the clear request."
                    : "The clear outcome is unconfirmed. Reload results to inspect the accepted search."
            }
        }
    }
    func loadSearchResults(start: UInt64, from page: AtlasSavedSearchPage? = nil) {
        guard searchIsCurrent, let info, let accepted = searchPage, let generation = queryGeneration,
              start <= accepted.retained else { return }
        if let page {
            guard page.hasSameSummary(as: accepted) else { return }
        }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let next = try AtlasSavedSearchPage.decode(await model.run(lease,
                .results(generation: generation, start: start, limit: 64), flag: flag),
                identity: info.identity, generation: generation, needle: accepted.needle, start: start, limit: 64,
                countAllMatches: accepted.counting.all, caseSensitive: accepted.caseSensitive)
            guard next.hasSameSummary(as: accepted) else {
                throw AtlasSavedError.invalidResponse
            }
            guard model.session === lease, !model.closed, model.queryGeneration == generation else { throw AtlasSavedError.canceled }
            model.searchPage = next
            model.notice = "Showing \(next.hits.count) of \(next.retained) retained matches. Result pages read metadata only."
        }
    }
    func reloadSearchResults() {
        if searchMode == .expression { reloadExpressionResults(); return }
        if searchMode.structure != nil { reloadStructureResults(); return }
        perform { model, _ in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let recovered = await model.reconcileSearch(lease)
            model.notice = recovered ? "Reloaded the accepted search without rescanning source."
                : "The accepted search is unavailable. Saved members and documentation links remain available independently."
        }
    }
    private func reconcileSearch(_ lease: AtlasSavedSession) async -> Bool {
        guard !closed, session === lease, let previous = info else { return false }
        let flag = AtlasSearchCancellation()
        do {
            let current = try AtlasSavedInfo.decode(await run(lease, .info, flag: flag),
                handle: lease.handle, expectedPath: previous.path.bytes, identity: previous.identity)
            guard !closed, session === lease else { return false }
            try acceptInfo(current, lease: lease); queryGeneration = current.queryGeneration
            guard let generation = current.queryGeneration else { searchPage = nil; return true }
            lastQueryAttempt = max(lastQueryAttempt, generation)
            if searchPage?.generation == generation && searchPage?.identity == current.identity { return true }
            let accepted = try AtlasSavedSearchPage.decode(await run(lease,
                .results(generation: generation, start: 0, limit: 64), flag: flag),
                identity: current.identity, generation: generation, needle: nil, start: 0, limit: 64)
            guard !closed, session === lease else { return false }
            searchPage = accepted; return true
        } catch {
            queryGeneration = nil; searchPage = nil
            return false
        }
    }

    private func searchExpression() {
        guard canOperate, let info else { return }
        if searchInput.isEmpty { clearExpression(); return }
        guard validSearchInput else {
            notice = "Enter 1–1024 UTF-8 bytes of expression syntax without NUL. The first positive term names matches."; return
        }
        let query = searchInput, counting = countAllMatches
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextExpressionGeneration()
            model.notice = "Checking required and excluded terms across captured members…"
            do {
                let request: AtlasSavedRequest = counting
                    ? .countedExpressionSearch(generation: generation, query: query, matches: 4096,
                        scanBytes: AtlasSavedSearchWire.maximumSourceBytes)
                    : .expressionSearch(generation: generation, query: query, matches: 4096,
                        scanBytes: AtlasSavedSearchWire.maximumSourceBytes)
                let page = try AtlasSavedExpressionPage.decode(await model.run(lease, request, flag: flag),
                    identity: info.identity, generation: generation, expression: query, start: 0, limit: 64,
                    countAllMatches: counting)
                guard page.counting.maximum == 4096 else { throw AtlasSavedError.invalidResponse }
                guard model.session === lease, !model.closed else { throw AtlasSavedError.canceled }
                model.expressionGeneration = generation; model.expressionPage = page
                model.notice = page.complete ? "Expression search finished over the captured scope."
                    : "Expression coverage is partial. Inspect source availability, work budget and retained-match limits."
            } catch {
                let failure = Self.message(error)
                let recovered = await model.reconcileExpression(lease)
                model.notice = recovered
                    ? "\(failure) Reloaded the accepted expression without repeating its scan."
                    : "\(failure) The accepted expression could not be verified; reload results before opening matches."
            }
        }
    }
    private func clearExpression() {
        guard canOperate, let info else { return }
        searchInput = ""
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextExpressionGeneration()
            do {
                try AtlasSavedExpressionPage.decodeClear(await model.run(lease, .clearExpression(generation: generation), flag: flag),
                    identity: info.identity, generation: generation)
                guard model.session === lease, !model.closed else { throw AtlasSavedError.canceled }
                model.expressionPage = nil; model.expressionGeneration = nil
                model.notice = "Cleared expression results. Literal results, documentation links and captured readers remain available."
            } catch {
                let recovered = await model.reconcileExpression(lease)
                model.notice = recovered ? "Reloaded the accepted expression state after the clear request."
                    : "The expression clear outcome is unconfirmed. Reload results to inspect it."
            }
        }
    }
    func loadExpressionResults(start: UInt64, from page: AtlasSavedExpressionPage) {
        guard expressionIsCurrent, let accepted = expressionPage, page.hasSameSummary(as: accepted),
              page.start == accepted.start, start <= accepted.retained else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let next = try AtlasSavedExpressionPage.decode(await model.run(lease,
                .expressionResults(generation: page.generation, start: start, limit: 64), flag: flag),
                identity: page.identity, generation: page.generation, expression: page.expression, start: start, limit: 64,
                countAllMatches: page.counting.all)
            guard next.hasSameSummary(as: accepted) else { throw AtlasSavedError.invalidResponse }
            guard model.session === lease, !model.closed, model.expressionGeneration == page.generation,
                  next.primaryNeedle.utf8.elementsEqual(page.primaryNeedle.utf8) else { throw AtlasSavedError.canceled }
            model.expressionPage = next
            model.notice = "Showing \(next.hits.count) of \(next.retained) retained expression matches. Pagination reads metadata only."
        }
    }
    private func reloadExpressionResults() {
        perform { model, _ in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let recovered = await model.reconcileExpression(lease)
            model.notice = recovered ? "Reloaded the accepted expression without rescanning source."
                : "Expression state is unavailable. Literal results and documentation links remain independent."
        }
    }
    private func reconcileExpression(_ lease: AtlasSavedSession) async -> Bool {
        guard !closed, session === lease, let previous = info else { return false }
        let flag = AtlasSearchCancellation()
        do {
            let current = try AtlasSavedInfo.decode(await run(lease, .info, flag: flag), handle: lease.handle,
                expectedPath: previous.path.bytes, identity: previous.identity)
            guard !closed, session === lease, current.expressionStateKnown else { throw AtlasSavedError.invalidResponse }
            try acceptInfo(current, lease: lease); expressionGeneration = current.expressionGeneration
            lastExpressionAttempt = max(lastExpressionAttempt, current.lastExpressionAttempt)
            guard let generation = current.expressionGeneration else { expressionPage = nil; return true }
            if expressionPage?.generation == generation && expressionPage?.identity == current.identity { return true }
            let page = try AtlasSavedExpressionPage.decode(await run(lease,
                .expressionResults(generation: generation, start: 0, limit: 64), flag: flag),
                identity: current.identity, generation: generation, expression: nil, start: 0, limit: 64)
            guard !closed, session === lease else { throw AtlasSavedError.canceled }
            expressionPage = page; return true
        } catch {
            expressionGeneration = nil; expressionPage = nil; return false
        }
    }
    private func searchStructure() {
        guard canOperate, let info, searchMode.structure != nil else { return }
        if searchInput.isEmpty { clearStructure(); return }
        guard validSearchInput else {
            notice = "Enter 1–1024 UTF-8 bytes without NUL. Occurrences require one exact identifier token."; return
        }
        let needle = searchInput, mode = searchMode, nameMode = searchMode.effectiveNameMode(searchNameMode)
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextStructureGeneration()
            model.notice = "Extracting \(mode.label.lowercased()) from verified saved members…"
            do {
                let page = try AtlasSavedStructurePage.decode(await model.run(lease,
                    .structureSearch(generation: generation, mode: mode, nameMode: nameMode, query: needle,
                        matches: AtlasSavedSearchWire.maximumHits, sourceBytes: AtlasSavedSearchWire.maximumSourceBytes), flag: flag),
                    identity: info.identity, generation: generation, mode: mode, nameMode: nameMode,
                    needle: needle, start: 0, limit: 64)
                guard model.session === lease, !model.closed else { throw AtlasSavedError.canceled }
                model.structureGeneration = generation; model.structurePage = page
                model.notice = page.complete ? "Finished extraction across the captured manifest. Results retain their stated evidence level."
                    : "Extraction coverage is partial. Inspect unsupported members, limits and omitted metadata."
            } catch {
                let failure = Self.message(error)
                let recovered = await model.reconcileStructure(lease)
                model.notice = recovered ? "\(failure) Reloaded accepted structural results without repeating extraction."
                    : "\(failure) Structural state could not be verified. Reload before opening results."
            }
        }
    }
    private func clearStructure() {
        guard canOperate, let info else { return }
        searchInput = ""
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextStructureGeneration()
            do {
                try AtlasSavedStructurePage.decodeClear(await model.run(lease, .clearStructure(generation: generation), flag: flag),
                    identity: info.identity, generation: generation)
                guard model.session === lease, !model.closed else { throw AtlasSavedError.canceled }
                model.structurePage = nil; model.structureGeneration = nil
                model.notice = "Cleared structural results. Text results, documentation links and independent readers remain available."
            } catch {
                let recovered = await model.reconcileStructure(lease)
                model.notice = recovered ? "Reloaded structural state after the clear request."
                    : "The structural clear outcome is unconfirmed. Reload results to inspect it."
            }
        }
    }
    func loadStructureResults(start: UInt64, from page: AtlasSavedStructurePage) {
        guard structureIsCurrent, let accepted = structurePage, page.identity == accepted.identity,
              page.generation == accepted.generation, page.start == accepted.start,
              page.mode == accepted.mode, page.nameMode == accepted.nameMode,
              page.needle.utf8.elementsEqual(accepted.needle.utf8), start <= accepted.retained else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let next = try AtlasSavedStructurePage.decode(await model.run(lease,
                .structureResults(generation: page.generation, start: start, limit: 64), flag: flag),
                identity: page.identity, generation: page.generation, mode: page.mode, nameMode: page.nameMode,
                needle: page.needle, start: start, limit: 64)
            guard model.session === lease, !model.closed, model.structureGeneration == page.generation else {
                throw AtlasSavedError.canceled
            }
            model.structurePage = next
            model.notice = "Showing \(next.hits.count) of \(next.retained) structural results. Pagination reads metadata only."
        }
    }
    private func reloadStructureResults() {
        perform { model, _ in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let recovered = await model.reconcileStructure(lease)
            model.notice = recovered ? "Reloaded structural results without repeating extraction or reading live paths."
                : "Structural state is unavailable. Text results and documentation links remain independent."
        }
    }
    private func reconcileStructure(_ lease: AtlasSavedSession) async -> Bool {
        guard !closed, session === lease, let previous = info else { return false }
        let flag = AtlasSearchCancellation()
        do {
            let current = try AtlasSavedInfo.decode(await run(lease, .info, flag: flag), handle: lease.handle,
                expectedPath: previous.path.bytes, identity: previous.identity)
            guard !closed, session === lease, current.structureStateKnown else { throw AtlasSavedError.invalidResponse }
            try acceptInfo(current, lease: lease); structureGeneration = current.structureGeneration
            lastStructureAttempt = max(lastStructureAttempt, current.lastStructureAttempt)
            guard let generation = current.structureGeneration else { structurePage = nil; return true }
            if structurePage?.generation == generation && structurePage?.identity == current.identity { return true }
            let page = try AtlasSavedStructurePage.decode(await run(lease,
                .structureResults(generation: generation, start: 0, limit: 64), flag: flag),
                identity: current.identity, generation: generation, mode: nil, nameMode: nil, needle: nil, start: 0, limit: 64)
            guard !closed, session === lease else { throw AtlasSavedError.canceled }
            structurePage = page; return true
        } catch {
            structureGeneration = nil; structurePage = nil; return false
        }
    }
    func openStructureHit(_ item: AtlasSavedStructureHit, from page: AtlasSavedStructurePage,
        readerTransport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil,
        outline: AtlasReaderOutlineTransport? = nil, document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasSavedReaderCapture {
        guard structureIsCurrent, let accepted = structurePage, info?.identity == page.identity,
              accepted.identity == page.identity, structureGeneration == page.generation, accepted.generation == page.generation,
              accepted.start == page.start, accepted.mode == page.mode, accepted.nameMode == page.nameMode,
              accepted.needle.utf8.elementsEqual(page.needle.utf8),
              page.hits.contains(where: { $0.matches(item) }), accepted.hits.contains(where: { $0.matches(item) }) else {
            throw AtlasSavedError.invalidInput
        }
        return try await openReader(saved: page.identity, ordinal: item.hit.member, expected: nil,
            origin: .structureHit(generation: page.generation, mode: page.mode, nameMode: page.nameMode, needle: page.needle, hit: item),
            readerTransport: readerTransport, search: search, outline: outline, document: document)
    }

    func openExpressionHit(_ hit: AtlasSavedHit, from page: AtlasSavedExpressionPage, readerTransport: AtlasReaderTransport,
        search: AtlasReaderSearchTransport? = nil, outline: AtlasReaderOutlineTransport? = nil,
        document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasSavedReaderCapture {
        guard expressionIsCurrent, let accepted = expressionPage, info?.identity == page.identity,
              page.hasSameSummary(as: accepted),
              accepted.identity == page.identity, expressionGeneration == page.generation, accepted.generation == page.generation,
              accepted.start == page.start, accepted.expression.utf8.elementsEqual(page.expression.utf8),
              accepted.primaryNeedle.utf8.elementsEqual(page.primaryNeedle.utf8),
              page.hits.contains(where: { $0.matches(hit) }), accepted.hits.contains(where: { $0.matches(hit) }) else {
            throw AtlasSavedError.invalidInput
        }
        return try await openReader(saved: page.identity, ordinal: hit.member, expected: nil,
            origin: .expressionHit(generation: page.generation, expression: page.expression, primaryNeedle: page.primaryNeedle, hit: hit),
            readerTransport: readerTransport, search: search, outline: outline, document: document)
    }

    func openHit(_ hit: AtlasSavedHit, from page: AtlasSavedSearchPage, readerTransport: AtlasReaderTransport,
        search: AtlasReaderSearchTransport? = nil, outline: AtlasReaderOutlineTransport? = nil,
        document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasSavedReaderCapture {
        guard searchIsCurrent, let accepted = searchPage, info?.identity == page.identity,
              page.hasSameSummary(as: accepted),
              accepted.identity == page.identity, queryGeneration == page.generation, accepted.generation == page.generation,
              accepted.start == page.start, accepted.needle.utf8.elementsEqual(page.needle.utf8),
              page.hits.contains(where: { $0.matches(hit) }),
              accepted.hits.contains(where: { $0.matches(hit) }) else { throw AtlasSavedError.invalidInput }
        return try await openReader(saved: page.identity, ordinal: hit.member, expected: nil,
            origin: .hit(generation: page.generation, needle: page.needle, hit: hit, caseSensitive: page.caseSensitive),
            readerTransport: readerTransport, search: search, outline: outline, document: document)
    }

    func openMember(_ member: AtlasSavedMember, from identity: AtlasSavedIdentity, readerTransport: AtlasReaderTransport,
        search: AtlasReaderSearchTransport? = nil, outline: AtlasReaderOutlineTransport? = nil,
        document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasSavedReaderCapture {
        guard info?.identity == identity,
              selectedMember?.matches(member) == true || members?.items.contains(where: { $0.matches(member) }) == true,
              member.captured else { throw AtlasSavedError.invalidInput }
        return try await openReader(saved: identity, ordinal: member.id, expected: member, origin: .member,
            readerTransport: readerTransport, search: search, outline: outline, document: document)
    }
    /// An opened archive supplies this capability, never a saved path label.
    /// Closing/replacing it disables onward navigation without closing readers.
    func canFollowDocumentLinks(from source: AtlasSavedReaderCapture) -> Bool {
        canOperate && ownsDocumentSource(source)
    }
    private func ownsDocumentSource(_ source: AtlasSavedReaderCapture) -> Bool {
        guard !closed, let archive = source.archiveIdentity, let info, let session,
              info.identity == archive, session.handle == archive.owner, source.owner == archive.owner,
              let domain = readerDomain, source.reader.retainedRegistryDomain == domain,
              source.member.captured, let digest = source.member.digest,
              source.reader.retainedCaptureSHA256 == digest,
              let identity = source.reader.page?.identity,
              identity.pathBytes == source.member.path.bytes,
              identity.capturedBytes == source.member.observedBytes else { return false }
        return true
    }
    private func ownsDocumentLink(_ target: AtlasReaderDocumentLinkTarget, from source: AtlasSavedReaderCapture) -> Bool {
        ownsDocumentSource(source) && target.link.kind == .local
            && source.reader.page?.identity.matches(target.inventory.identity) == true
            && source.reader.documentPreview.allows(target)
            && target.link.evidence.start == 0 && target.link.evidence.end == source.member.observedBytes
    }

    /// Follow a verified preview occurrence through the existing saved-manifest
    /// relationship resolver. No member label is opened as a working-tree path.
    /// The bounded relationship inventory is prepared once on this deliberate
    /// action, if needed; later actions page retained metadata without parsing.
    func openDocumentLink(_ target: AtlasReaderDocumentLinkTarget, from source: AtlasSavedReaderCapture,
        readerTransport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil,
        outline: AtlasReaderOutlineTransport? = nil, document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasSavedReaderCapture {
        guard canFollowDocumentLinks(from: source), ownsDocumentLink(target, from: source),
              let archive = source.archiveIdentity,
              readerTransport.comparison?.domain == readerDomain else { throw AtlasSavedError.staleDocumentLink }
        try Task.checkCancellation()
        do {
            let (generation, link) = try await resolveDocumentLink(target, from: source)
            try Task.checkCancellation()
            guard canFollowDocumentLinks(from: source), ownsDocumentLink(target, from: source),
                  relationshipGeneration == generation, let targetMember = link.targetMember else {
                throw AtlasSavedError.staleDocumentLink
            }
            let opened = try await openReader(saved: archive, ordinal: targetMember, expected: nil,
                origin: .relationship(generation: generation, link: link, side: .target),
                readerTransport: readerTransport, search: search, outline: outline, document: document)
            guard !Task.isCancelled, ownsDocumentLink(target, from: source),
                  relationshipGeneration == generation else {
                opened.reader.close()
                throw AtlasSavedError.staleDocumentLink
            }
            notice = "Opened the linked captured member. Back returns to the original saved reader; no current-project file was read."
            return opened
        } catch {
            notice = Self.message(error)
            throw error
        }
    }

    private func resolveDocumentLink(_ target: AtlasReaderDocumentLinkTarget, from source: AtlasSavedReaderCapture)
        async throws -> (UInt64, AtlasSavedLink) {
        guard canOperate, ownsDocumentLink(target, from: source), let lease = session,
              let archive = source.archiveIdentity else { throw AtlasSavedError.staleDocumentLink }
        let flag = AtlasSearchCancellation()
        cancellation = flag; busy = true
        defer { finish() }
        return try await withTaskCancellationHandler(operation: {
            if relationshipGeneration == nil {
                let generation = try nextRelationshipGeneration()
                notice = "Preparing bounded documentation links from the saved archive…"
                do {
                    let page = try AtlasSavedRelationships.decode(await run(lease,
                        .prepareRelationships(generation: generation, links: 4096, bytes: AtlasSavedWire.relationshipBytes), flag: flag),
                        identity: archive, generation: generation, direction: .all, member: nil, start: 0, limit: 64, preparing: true)
                    guard session === lease, !flag.isCanceled, ownsDocumentLink(target, from: source) else {
                        throw AtlasSavedError.staleDocumentLink
                    }
                    relationshipGeneration = generation; relationships = page; direction = .all
                } catch {
                    // A missing/canceled reply may follow publication. Recover
                    // metadata once; never replay the build or a source open.
                    _ = await reconcileRelationships(lease)
                    throw error
                }
            }
            guard let generation = relationshipGeneration else { throw AtlasSavedError.unavailable }
            var first: UInt64 = 0
            var previous: AtlasSavedRelationships?
            var lastOccurrence: UInt64 = 0
            while true {
                try Task.checkCancellation()
                guard session === lease, !flag.isCanceled, ownsDocumentLink(target, from: source),
                      relationshipGeneration == generation else { throw AtlasSavedError.staleDocumentLink }
                let page = try AtlasSavedRelationships.decode(await run(lease,
                    .relationships(generation: generation, direction: .outgoing, member: source.member.id, start: first, limit: 64),
                    flag: flag), identity: archive, generation: generation, direction: .outgoing,
                    member: source.member.id, start: first, limit: 64)
                guard session === lease, !flag.isCanceled, ownsDocumentLink(target, from: source),
                      relationshipGeneration == generation,
                      previous.map({ page.matchesInventory($0) }) != false else { throw AtlasSavedError.staleDocumentLink }
                for link in page.links {
                    guard link.sourceOccurrence > lastOccurrence, link.sourceMember == source.member.id,
                          link.sourceDigest == source.member.digest, link.sourcePath.bytes == source.member.path.bytes,
                          link.evidenceEnd == source.member.observedBytes else { throw AtlasSavedError.invalidResponse }
                    lastOccurrence = link.sourceOccurrence
                    if link.sourceOccurrence == target.link.id {
                        guard link.matchesDocumentOccurrence(target, source: source.member) else {
                            throw AtlasSavedError.invalidResponse
                        }
                        guard link.canOpenTarget, link.targetMember != nil else { throw AtlasSavedError.unavailableDocumentTarget }
                        return (generation, link)
                    }
                }
                guard let next = page.next else { throw AtlasSavedError.unindexedDocumentLink }
                // Wire decoding already proves monotonic, <=4096 pagination.
                first = next; previous = page
            }
        }, onCancel: { flag.cancel(); lease.cancel() })
    }

    func openLink(_ link: AtlasSavedLink, from page: AtlasSavedRelationships, side: AtlasSavedLinkSide, readerTransport: AtlasReaderTransport,
        search: AtlasReaderSearchTransport? = nil, outline: AtlasReaderOutlineTransport? = nil,
        document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasSavedReaderCapture {
        guard let relationships, info?.identity == page.identity, relationships.identity == page.identity,
              relationshipGeneration == page.generation, relationships.generation == page.generation,
              relationships.start == page.start, relationships.direction == page.direction, relationships.member == page.member,
              page.links.contains(where: { $0.matches(link) }),
              relationships.links.contains(where: { $0.matches(link) }) else { throw AtlasSavedError.invalidInput }
        let member: UInt64
        if side == .evidence { member = link.sourceMember }
        else {
            guard link.canOpenTarget, let target = link.targetMember else { throw AtlasSavedError.invalidInput }
            member = target
        }
        return try await openReader(saved: page.identity, ordinal: member, expected: nil,
            origin: .relationship(generation: page.generation, link: link, side: side),
            readerTransport: readerTransport, search: search, outline: outline, document: document)
    }
    private func openReader(saved: AtlasSavedIdentity, ordinal: UInt64, expected: AtlasSavedMember?, origin: AtlasSavedReaderOrigin,
        readerTransport: AtlasReaderTransport, search: AtlasReaderSearchTransport?,
        outline: AtlasReaderOutlineTransport?, document: AtlasReaderDocumentTransport?) async throws -> AtlasSavedReaderCapture {
        guard canOperate, let lease = session, let info, info.identity == saved, let domain = readerDomain,
              readerTransport.comparison?.domain == domain else { throw AtlasSavedError.unavailable }
        try Task.checkCancellation()
        let imageDocument = try lease.documentTransport(document, archive: info.identity, sourceMember: ordinal)
        let flag = AtlasSearchCancellation()
        cancellation = flag; busy = true
        let reader = AtlasPagedReaderModel(transport: readerTransport, search: search, outline: outline, document: imageDocument)
        defer { finish() }
        do {
            return try await withTaskCancellationHandler(operation: {
                // Metadata is obtained before empty-reader admission, and it
                // cannot read a source payload. Payload I/O starts only inside
                // the admitted native reader handoff.
                let page = try AtlasSavedMembers.decode(await run(lease, .members(start: ordinal, limit: 1), flag: flag),
                    identity: info.identity, start: ordinal, limit: 1)
                guard let member = page.items.first, member.captured,
                      expected == nil || expected!.matches(member), !flag.isCanceled else { throw AtlasSavedError.invalidResponse }
                if case .relationship(_, let link, let side) = origin {
                    if side == .evidence {
                        guard member.id == link.sourceMember, member.path.bytes == link.sourcePath.bytes,
                              member.digest == link.sourceDigest, member.observedBytes == link.evidenceEnd else {
                            throw AtlasSavedError.invalidResponse
                        }
                    } else {
                        guard member.id == link.targetMember, member.path.bytes == link.targetPath?.bytes else {
                            throw AtlasSavedError.invalidResponse
                        }
                    }
                }
                if case .importLink(_, let link, let side) = origin {
                    guard link.matches(member, side: side) else { throw AtlasSavedError.invalidResponse }
                }
                let contextStart: UInt64
                if let hit = origin.searchHit {
                    guard member.id == hit.member, member.file == hit.file, member.file == hit.revision,
                          member.path.bytes == hit.path.bytes, member.digest == hit.digest,
                          hit.start < hit.end, hit.end <= member.observedBytes else { throw AtlasSavedError.invalidResponse }
                    contextStart = hit.start > 132 ? hit.start - 132 : 0
                } else if case .relationship(_, let link, .target) = origin, let range = link.targetRange {
                    guard range.end <= member.observedBytes else { throw AtlasSavedError.invalidResponse }
                    contextStart = range.contextStart
                } else if case .importLink(_, let link, .evidence) = origin {
                    contextStart = link.evidence.contextStart
                } else { contextStart = 0 }
                let receipt = AtlasSavedReaderReceiptBox()
                try await reader.openRetained(pathBytes: member.path.bytes, offset: contextStart, registryDomain: domain) { destination in
                    guard !flag.isCanceled else { throw AtlasSavedError.canceled }
                    let request: AtlasSavedRequest
                    switch origin {
                    case .member: request = .openMember(reader: destination, member: member.id)
                    case .pathHit(let generation, let hit):
                        request = .openPathHit(reader: destination, generation: generation, hit: hit.id)
                    case .relationship(let generation, let link, let side):
                        request = .openRelationship(reader: destination, generation: generation, link: link.id, side: side)
                    case .importLink(let generation, let link, let side):
                        request = .openImport(reader: destination, generation: generation, importID: link.id, side: side)
                    case .hit(let generation, _, let hit, _):
                        request = .openHit(reader: destination, generation: generation, hit: hit.id)
                    case .expressionHit(let generation, _, _, let hit):
                        request = .openExpressionHit(reader: destination, generation: generation, hit: hit.id)
                    case .structureHit(let generation, _, _, _, let item):
                        request = .openStructureHit(reader: destination, generation: generation, hit: item.id)
                    }
                    return try withExtendedLifetime(lease) {
                        let accepted = try AtlasSavedReaderReceipt.decode(lease.transport.run(lease.handle, request),
                            saved: info.identity, member: member, origin: origin, destination: destination)
                        try receipt.set(accepted)
                        return accepted.readerJSON
                    }
                }
                try Task.checkCancellation()
                guard !closed, !flag.isCanceled, session === lease, self.info?.identity == info.identity else {
                    throw AtlasSavedError.canceled
                }
                let accepted = try receipt.accepted()
                try accepted.validateInitialPage(reader.page)
                notice = "Opened the exact verified saved member. The independent reader remains usable after this archive is closed."
                return AtlasSavedReaderCapture(reader: reader, owner: info.identity.owner, member: member, origin: origin,
                    match: accepted.match, archiveIdentity: info.identity)
            }, onCancel: { flag.cancel(); lease.cancel() })
        } catch {
            // A lost/canceled receipt may have initialized the destination. It
            // is always retired; this operation never creates a second reader.
            reader.close()
            notice = Self.message(error)
            throw error
        }
    }

    func cancel() {
        guard busy else { return }
        cancellation?.cancel(); session?.cancel(); pending?.cancel()
        notice = "Cancel requested. Waiting for entered saved-archive work to drain."
    }
    func closeArchive() {
        guard canOperate else { return }
        session?.revokeDocumentImages(); session = nil; clearMetadata()
        notice = "Closed the archive. Independent readers and already loaded images keep their captured bytes; new saved-image loads require the opened archive."
    }
    func close() {
        guard !closed else { return }
        session?.revokeDocumentImages(); pending?.revokeDocumentImages()
        closed = true; cancel(); task?.cancel(); clearMetadata()
        if !busy { session = nil; pending = nil }
    }
    private func clearMetadata() {
        info = nil; members = nil; selectedMember = nil; relationships = nil; relationshipGeneration = nil
        direction = .all; lastRelationshipAttempt = 0; memberOffsetInput = "0"
        imports = nil; importGeneration = nil; importDirection = .all; lastImportAttempt = 0
        pathPage = nil; pathGeneration = nil; pathInput = ""; lastPathAttempt = 0
        searchPage = nil; queryGeneration = nil; lastQueryAttempt = 0; searchInput = ""
        expressionPage = nil; expressionGeneration = nil; lastExpressionAttempt = 0
        structurePage = nil; structureGeneration = nil; lastStructureAttempt = 0
        indexPath = nil; indexStateKnown = true; lastIndexAttempt = 0; indexPinInput = ""
    }
    private func run(_ lease: AtlasSavedSession, _ request: AtlasSavedRequest,
        flag: AtlasSearchCancellation) async throws -> String? {
        try await io.perform(cancellation: flag, keepingAlive: [lease]) { lease.transport.run(lease.handle, request) }
    }
    private func perform(_ work: @escaping @MainActor (AtlasSavedRepositoryModel, AtlasSearchCancellation) async throws -> Void) {
        guard canOperate else { return }
        let flag = AtlasSearchCancellation()
        cancellation = flag; busy = true
        task = Task {
            do { try await work(self, flag) }
            catch { notice = Self.message(error) }
            finish()
        }
    }
    private func finish() {
        busy = false; cancellation = nil; task = nil; pending = nil
        if closed { session = nil }
    }
    private func nextRelationshipGeneration() throws -> UInt64 {
        let previous = max(lastRelationshipAttempt, relationshipGeneration ?? 0)
        guard previous < UInt64.max else { throw AtlasSavedError.unavailable }
        lastRelationshipAttempt = previous + 1; return lastRelationshipAttempt
    }
    private func nextQueryGeneration() throws -> UInt64 {
        let previous = max(lastQueryAttempt, queryGeneration ?? 0)
        guard previous < UInt64.max else { throw AtlasSavedError.unavailable }
        lastQueryAttempt = previous + 1; return lastQueryAttempt
    }
    private func nextImportGeneration() throws -> UInt64 {
        let previous = max(lastImportAttempt, max(importGeneration ?? 0, info?.lastImportAttempt ?? 0))
        guard previous < UInt64.max else { throw AtlasSavedError.unavailable }
        lastImportAttempt = previous + 1; return lastImportAttempt
    }
    private func nextPathGeneration() throws -> UInt64 {
        let previous = max(lastPathAttempt, max(pathGeneration ?? 0, info?.lastPathAttempt ?? 0))
        guard previous < UInt64.max else { throw AtlasSavedError.unavailable }
        lastPathAttempt = previous + 1; return lastPathAttempt
    }
    private func nextExpressionGeneration() throws -> UInt64 {
        let previous = max(lastExpressionAttempt, max(expressionGeneration ?? 0, info?.lastExpressionAttempt ?? 0))
        guard previous < UInt64.max else { throw AtlasSavedError.unavailable }
        lastExpressionAttempt = previous + 1; return lastExpressionAttempt
    }
    private func nextStructureGeneration() throws -> UInt64 {
        let previous = max(lastStructureAttempt, max(structureGeneration ?? 0, info?.lastStructureAttempt ?? 0))
        guard previous < UInt64.max else { throw AtlasSavedError.unavailable }
        lastStructureAttempt = previous + 1; return lastStructureAttempt
    }
    private func nextIndexGeneration() throws -> UInt64 {
        let previous = max(lastIndexAttempt, info?.lastIndexAttempt ?? 0)
        guard previous < UInt64.max else { throw AtlasSavedError.unavailable }
        lastIndexAttempt = previous + 1; return lastIndexAttempt
    }
    private static func message(_ error: Error) -> String {
        if let error = error as? AtlasSavedError { return error.message }
        if let error = error as? AtlasReaderError { return error.message }
        if error is CancellationError { return AtlasSavedError.canceled.message }
        if let error = error as? AtlasProjectIOError, case .canceled = error { return AtlasSavedError.canceled.message }
        return AtlasSavedError.invalidResponse.message
    }
}
