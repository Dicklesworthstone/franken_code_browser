import Foundation
import Observation
import Dispatch

/// This immutable lifetime container is never operated on by a worker. The
/// archive descriptor and its selected-file grant drain together off the UI.
private final class AtlasSavedAccess: @unchecked Sendable {
    let reference: AnyObject?
    init(_ reference: AnyObject?) { self.reference = reference }
}
private final class AtlasSavedSession: @unchecked Sendable {
    let handle: UInt64
    let transport: AtlasSavedTransport
    private let access: AtlasSavedAccess
    private static let retirement = DispatchQueue(label: "dev.frankencode.browser.saved-retirement", qos: .utility)
    init(transport: AtlasSavedTransport, access: AnyObject?) throws {
        let handle = transport.create()
        guard handle != 0 else { throw AtlasSavedError.unavailable }
        self.handle = handle; self.transport = transport; self.access = AtlasSavedAccess(access)
    }
    func cancel() { _ = transport.cancel(handle) }
    deinit {
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
    private(set) var searchPage: AtlasSavedSearchPage?
    private(set) var queryGeneration: UInt64?
    private(set) var expressionPage: AtlasSavedExpressionPage?
    private(set) var expressionGeneration: UInt64?
    private(set) var busy = false
    private(set) var closed = false
    private(set) var notice = "Open an FCBS saved repository to browse its captured members and documentation links."
    var memberOffsetInput = "0"
    var searchInput = ""
    var searchMode: AtlasSavedSearchMode = .literal
    @ObservationIgnored private let transport: AtlasSavedTransport
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private var session: AtlasSavedSession?
    @ObservationIgnored private var pending: AtlasSavedSession?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var cancellation: AtlasSearchCancellation?
    @ObservationIgnored private var lastRelationshipAttempt: UInt64 = 0
    @ObservationIgnored private var lastQueryAttempt: UInt64 = 0
    @ObservationIgnored private var lastExpressionAttempt: UInt64 = 0

    init(transport: AtlasSavedTransport) { self.transport = transport }
    var canOperate: Bool { !busy && !closed }
    var readerDomain: UUID? { transport.readerDomain }
    var validSearchInput: Bool { AtlasSavedSearchWire.validNeedle(searchInput) }
    var searchIsCurrent: Bool {
        guard searchMode == .literal, let searchPage, info?.identity == searchPage.identity,
              queryGeneration == searchPage.generation else { return false }
        return searchInput.utf8.elementsEqual(searchPage.needle.utf8)
    }
    var expressionIsCurrent: Bool {
        guard searchMode == .expression, let expressionPage, info?.identity == expressionPage.identity,
              expressionGeneration == expressionPage.generation else { return false }
        return searchInput.utf8.elementsEqual(expressionPage.expression.utf8)
    }

    func open(path: String, accessLease: AnyObject?) {
        guard canOperate, path.hasPrefix("/"), path.utf8.count <= 16_384, !path.utf8.contains(0) else { return }
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
                let page = try AtlasSavedMembers.decode(await model.run(candidate, .members(start: 0, limit: 64), flag: flag),
                    identity: opened.identity, start: 0, limit: 64)
                guard !flag.isCanceled, !model.closed, model.pending === candidate else { throw AtlasSavedError.canceled }
                model.session = candidate; model.pending = nil
                model.info = opened; model.members = page; model.selectedMember = page.items.first
                model.relationships = nil; model.relationshipGeneration = nil; model.direction = .all
                model.lastRelationshipAttempt = 0; model.memberOffsetInput = "0"
                model.searchPage = nil; model.queryGeneration = nil; model.lastQueryAttempt = 0; model.searchInput = ""
                model.expressionPage = nil; model.expressionGeneration = nil; model.lastExpressionAttempt = 0
                model.notice = "Opened the saved manifest. Search all captured text or build documentation links explicitly when ready."
            } catch {
                if model.pending === candidate { model.pending = nil; candidate.cancel() }
                throw error
            }
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
    func selectMember(_ member: AtlasSavedMember, from identity: AtlasSavedIdentity) {
        guard canOperate, info?.identity == identity, members?.identity == identity,
              members?.items.contains(where: { $0.matches(member) }) == true else { return }
        selectedMember = member
        if direction != .all, relationshipGeneration != nil {
            relationships = nil
            loadRelationships(direction: direction)
        }
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
            info = current; relationshipGeneration = current.relationshipGeneration
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
        guard canOperate, let info else { return }
        if searchInput.isEmpty { clearSearch(); return }
        guard validSearchInput else {
            notice = "Enter 1–1024 UTF-8 bytes of literal text, without NUL. Search is case sensitive and does not normalize Unicode."; return
        }
        let needle = searchInput
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextQueryGeneration()
            model.notice = "Searching decoded text across the captured archive…"
            do {
                let page = try AtlasSavedSearchPage.decode(await model.run(lease,
                    .search(generation: generation, needle: needle, matches: 4096), flag: flag),
                    identity: info.identity, generation: generation, needle: needle, start: 0, limit: 64, searching: true)
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
            guard page.identity == accepted.identity, page.generation == generation,
                  page.needle.utf8.elementsEqual(accepted.needle.utf8) else { return }
        }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let next = try AtlasSavedSearchPage.decode(await model.run(lease,
                .results(generation: generation, start: start, limit: 64), flag: flag),
                identity: info.identity, generation: generation, needle: accepted.needle, start: start, limit: 64)
            guard model.session === lease, !model.closed, model.queryGeneration == generation else { throw AtlasSavedError.canceled }
            model.searchPage = next
            model.notice = "Showing \(next.hits.count) of \(next.retained) retained matches. Result pages read metadata only."
        }
    }
    func reloadSearchResults() {
        if searchMode == .expression { reloadExpressionResults(); return }
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
            info = current; queryGeneration = current.queryGeneration
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
        let query = searchInput
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let generation = try model.nextExpressionGeneration()
            model.notice = "Checking required and excluded terms across captured members…"
            do {
                let page = try AtlasSavedExpressionPage.decode(await model.run(lease,
                    .expressionSearch(generation: generation, query: query, matches: 4096,
                        scanBytes: AtlasSavedSearchWire.maximumSourceBytes), flag: flag),
                    identity: info.identity, generation: generation, expression: query, start: 0, limit: 64)
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
        guard expressionIsCurrent, let accepted = expressionPage, page.identity == accepted.identity,
              page.generation == accepted.generation, page.start == accepted.start,
              page.expression.utf8.elementsEqual(accepted.expression.utf8),
              page.primaryNeedle.utf8.elementsEqual(accepted.primaryNeedle.utf8), start <= accepted.retained else { return }
        perform { model, flag in
            guard let lease = model.session else { throw AtlasSavedError.unavailable }
            let next = try AtlasSavedExpressionPage.decode(await model.run(lease,
                .expressionResults(generation: page.generation, start: start, limit: 64), flag: flag),
                identity: page.identity, generation: page.generation, expression: page.expression, start: start, limit: 64)
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
            info = current; expressionGeneration = current.expressionGeneration
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
    func openExpressionHit(_ hit: AtlasSavedHit, from page: AtlasSavedExpressionPage, readerTransport: AtlasReaderTransport,
        search: AtlasReaderSearchTransport? = nil, outline: AtlasReaderOutlineTransport? = nil,
        document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasSavedReaderCapture {
        guard expressionIsCurrent, let accepted = expressionPage, info?.identity == page.identity,
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
              accepted.identity == page.identity, queryGeneration == page.generation, accepted.generation == page.generation,
              accepted.start == page.start, accepted.needle.utf8.elementsEqual(page.needle.utf8),
              page.hits.contains(where: { $0.matches(hit) }),
              accepted.hits.contains(where: { $0.matches(hit) }) else { throw AtlasSavedError.invalidInput }
        return try await openReader(saved: page.identity, ordinal: hit.member, expected: nil,
            origin: .hit(generation: page.generation, needle: page.needle, hit: hit),
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
        let flag = AtlasSearchCancellation()
        cancellation = flag; busy = true
        let reader = AtlasPagedReaderModel(transport: readerTransport, search: search, outline: outline, document: document)
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
                let contextStart: UInt64
                if let hit = origin.searchHit {
                    guard member.id == hit.member, member.file == hit.file, member.file == hit.revision,
                          member.path.bytes == hit.path.bytes, member.digest == hit.digest,
                          hit.start < hit.end, hit.end <= member.observedBytes else { throw AtlasSavedError.invalidResponse }
                    contextStart = hit.start > 132 ? hit.start - 132 : 0
                } else { contextStart = 0 }
                let receipt = AtlasSavedReaderReceiptBox()
                try await reader.openRetained(pathBytes: member.path.bytes, offset: contextStart, registryDomain: domain) { destination in
                    guard !flag.isCanceled else { throw AtlasSavedError.canceled }
                    let request: AtlasSavedRequest
                    switch origin {
                    case .member: request = .openMember(reader: destination, member: member.id)
                    case .relationship(let generation, let link, let side):
                        request = .openRelationship(reader: destination, generation: generation, link: link.id, side: side)
                    case .hit(let generation, _, let hit):
                        request = .openHit(reader: destination, generation: generation, hit: hit.id)
                    case .expressionHit(let generation, _, _, let hit):
                        request = .openExpressionHit(reader: destination, generation: generation, hit: hit.id)
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
                    match: accepted.match)
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
        session = nil; clearMetadata()
        notice = "Closed the archive. Independent readers and Desk captures keep their own verified bytes."
    }
    func close() {
        guard !closed else { return }
        closed = true; cancel(); task?.cancel(); clearMetadata()
        if !busy { session = nil; pending = nil }
    }
    private func clearMetadata() {
        info = nil; members = nil; selectedMember = nil; relationships = nil; relationshipGeneration = nil
        direction = .all; lastRelationshipAttempt = 0; memberOffsetInput = "0"
        searchPage = nil; queryGeneration = nil; lastQueryAttempt = 0; searchInput = ""
        expressionPage = nil; expressionGeneration = nil; lastExpressionAttempt = 0
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
    private func nextExpressionGeneration() throws -> UInt64 {
        let previous = max(lastExpressionAttempt, max(expressionGeneration ?? 0, info?.lastExpressionAttempt ?? 0))
        guard previous < UInt64.max else { throw AtlasSavedError.unavailable }
        lastExpressionAttempt = previous + 1; return lastExpressionAttempt
    }
    private static func message(_ error: Error) -> String {
        if let error = error as? AtlasSavedError { return error.message }
        if let error = error as? AtlasReaderError { return error.message }
        if error is CancellationError { return AtlasSavedError.canceled.message }
        if let error = error as? AtlasProjectIOError, case .canceled = error { return AtlasSavedError.canceled.message }
        return AtlasSavedError.invalidResponse.message
    }
}
