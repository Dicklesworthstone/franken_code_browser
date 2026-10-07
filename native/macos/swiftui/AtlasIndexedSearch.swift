import Foundation

/// C-boundary injection only. The Rust atlas owns discovery, capture, trigrams,
/// fallback verification and byte ranges. No native matcher or persisted cache.
struct AtlasIndexedSearchTransport: Sendable {
    let create: @Sendable () -> UInt64
    let open: @Sendable (UInt64, String, AtlasSearchCancellation) throws -> String?
    let indexBegin: @Sendable (UInt64, UInt64) throws -> String?
    let indexStep: @Sendable (UInt64, UInt64, AtlasSearchCancellation) throws -> String?
    let queryBegin: @Sendable (UInt64, UInt64, UInt64, String) throws -> String?
    let queryStep: @Sendable (UInt64, UInt64, AtlasSearchCancellation) throws -> String?
    let page: @Sendable (UInt64, UInt64, UInt64, UInt64) throws -> String?
    let sourceReader: AtlasIndexedSource.Import
    let close: @Sendable (UInt64) -> Bool
    let retirementFailed: @Sendable () -> Void
}

/// An explicit captured-index session. The app serializes run() on its existing
/// one-active/one-newest search coordinator. A concurrent caller is refused,
/// never added to an unbounded queue. Construction is inert. Replace this owner
/// on refresh/root-grant changes: selected sources pin the old immutable index.
/// Completed preparation steps survive query edits; an uncertain foreign step
/// retires the private builder. No partial index is advertised as queryable.
final class AtlasIndexedSearch: @unchecked Sendable {
    private final class Session {
        let owner: AtlasSearchCaptureOwner
        let root: String
        let grant: ObjectIdentifier?
        let basis: AtlasIndexedBasis
        var lastGeneration: UInt64
        init(owner: AtlasSearchCaptureOwner, root: String, grant: ObjectIdentifier?, basis: AtlasIndexedBasis) {
            self.owner = owner; self.root = root; self.grant = grant; self.basis = basis
            lastGeneration = basis.index
        }
    }
    /// Only the last validated receipt is resumable. The actual source bytes
    /// and builder remain in Rust, bounded by the existing per-index quotas.
    private final class Preparation {
        let owner: AtlasSearchCaptureOwner
        let root: String
        let grant: ObjectIdentifier?
        let handle, layout: UInt64
        let files: Int
        let discovery: Bool
        var page: AtlasIndexBuildPage
        init(owner: AtlasSearchCaptureOwner, root: String, grant: ObjectIdentifier?, handle: UInt64,
             layout: UInt64, files: Int, discovery: Bool, page: AtlasIndexBuildPage) {
            self.owner = owner; self.root = root; self.grant = grant; self.handle = handle
            self.layout = layout; self.files = files; self.discovery = discovery; self.page = page
        }
    }
    private let transport: AtlasIndexedSearchTransport
    private let serial = NSLock()
    private var cached: Session?
    private var preparing: Preparation?
    init(transport: AtlasIndexedSearchTransport) { self.transport = transport }

    func run(root: String, query: String, cancellation: AtlasSearchCancellation,
             access: AtlasSearchAccessLease, progress: @Sendable (AtlasSearchReport) -> Void) throws -> AtlasSearchReport {
        _ = try AtlasSearchInput(root: root, query: query)
        try check(cancellation)
        guard serial.try() else { throw AtlasSearchError.unavailable }
        defer { serial.unlock() }
        do {
            let grant = access.reference.map(ObjectIdentifier.init)
            let reused = cached.map { $0.root.utf8.elementsEqual(root.utf8) && $0.grant == grant } == true
            if !reused {
                cached = nil
                // Publish a successfully finished index before the next cancel
                // gate. Cancellation can suppress query delivery, not make an
                // acknowledged complete source universe need another capture.
                cached = try build(root: root, access: access, cancellation: cancellation, progress: progress)
            }
            guard let session = cached else { throw AtlasSearchError.unavailable }
            let (generation, overflow) = session.lastGeneration.addingReportingOverflow(1)
            guard !overflow else { throw AtlasSearchError.identityExhausted }
            session.lastGeneration = generation // Failed/canceled attempts cannot reuse a query ID.
            let basis = session.basis, owner = session.owner
            try check(cancellation)
            var page = try AtlasIndexedQueryPage(owner.call { try transport.queryBegin($0, generation, basis.index, query) },
                basis: basis, generation: generation, needle: query, command: "begin-indexed")
            var result = AtlasIndexedAccumulator()
            var lastPublished = -Double.infinity
            var publishedHits = 0
            while true {
                try check(cancellation)
                try result.head(page, basis: basis)
                while result.hits.count < page.state.retained {
                    try check(cancellation)
                    let start = result.hits.count
                    let tail = try AtlasIndexedQueryPage(owner.call { try transport.page($0, generation, UInt64(start), 128) },
                        basis: basis, generation: generation, needle: query, command: "page")
                    try result.append(tail, start: start, limit: 128)
                }
                let state = page.state
                let capture = result.hits.isEmpty ? nil : AtlasIndexedSource.capture(owner: owner, basis: basis,
                    root: root, needle: query, hits: result.hits, importReader: transport.sourceReader)
                let report = AtlasSearchReport(hits: result.hits.map(\.hit), complete: state.complete,
                    truncated: state.truncated, unavailableFiles: state.unavailable, unsupportedFiles: 0,
                    matchesSeen: state.matches,
                    progress: AtlasSearchProgress(isRunning: state.running, examinedFiles: state.examined, cataloguedFiles: basis.files),
                    streamID: result.id, capture: capture,
                    indexStatus: AtlasSearchIndexStatus(building: false, reused: reused,
                        capturedFiles: basis.capturedFiles, capturedBytes: basis.sourceBytes,
                        skippedFiles: state.skipped, verifiedFiles: state.scanned, verificationBytes: state.verificationBytes))
                try check(cancellation)
                if !state.running { return report }
                let now = ProcessInfo.processInfo.systemUptime
                if now - lastPublished >= 0.1 || (publishedHits == 0 && !result.hits.isEmpty) {
                    progress(report); lastPublished = now; publishedHits = result.hits.count
                }
                try check(cancellation)
                page = try AtlasIndexedQueryPage(owner.call { try transport.queryStep($0, generation, cancellation) },
                    basis: basis, generation: generation, needle: query, command: "step")
            }
        } catch {
            if cancellation.isCanceled { throw AtlasSearchError.canceled }
            if let error = error as? AtlasSearchError { throw error }
            throw AtlasSearchError.invalidResponse
        }
    }

    private func beginBuild(root: String, access: AtlasSearchAccessLease,
                            cancellation: AtlasSearchCancellation) throws -> Preparation {
        let handle = transport.create()
        let owner = try AtlasSearchCaptureOwner(handle: handle, close: transport.close, retirementFailed: transport.retirementFailed)
        owner.retainAccess(access)
        try check(cancellation)
        let opened = try AtlasIndexWire.decode(owner.call { try transport.open($0, root, cancellation) })
        try opened.header(schema: "fcb.atlas-session/1", command: "info", owner: handle)
        let files = try opened.count("catalogued_files", upTo: 4096)
        let discovery = try opened.flag("discovery_complete"), layout = try opened.number("layout_revision")
        guard layout > 0, try opened.text("scope") == "all", try opened.number("workspace_files") == UInt64(files),
              try opened.text("retention") == "frozen-catalog-and-spatial-index" else { throw AtlasIndexWire.invalid() }
        try check(cancellation)
        let page = try AtlasIndexBuildPage(owner.call { try transport.indexBegin($0, 1) },
            owner: handle, layout: layout, files: files, discovery: discovery, command: "index-begin")
        guard page.steps == 0, page.examined == 0, page.bytes == 0, page.readBytes == 0, page.running else { throw AtlasIndexWire.invalid() }
        return Preparation(owner: owner, root: root, grant: access.reference.map(ObjectIdentifier.init),
            handle: handle, layout: layout, files: files, discovery: discovery, page: page)
    }

    private func build(root: String, access: AtlasSearchAccessLease, cancellation: AtlasSearchCancellation,
                       progress: @Sendable (AtlasSearchReport) -> Void) throws -> Session {
        let grant = access.reference.map(ObjectIdentifier.init)
        if preparing.map({ $0.root.utf8.elementsEqual(root.utf8) && $0.grant == grant }) != true {
            preparing = nil
            preparing = try beginBuild(root: root, access: access, cancellation: cancellation)
        }
        guard let preparation = preparing else { throw AtlasSearchError.unavailable }
        // A replacement request gets a new report stream, even when it resumes
        // the same builder. Old-query row/selection authority is never reused.
        let stream = UUID()
        var lastPublished = -Double.infinity
        while preparation.page.running {
            try check(cancellation)
            let page = preparation.page
            let now = ProcessInfo.processInfo.systemUptime
            if now - lastPublished >= 0.1 {
                progress(AtlasSearchReport(hits: [], complete: false, truncated: false,
                    unavailableFiles: page.unavailable, unsupportedFiles: 0, matchesSeen: 0,
                    progress: AtlasSearchProgress(isRunning: true, examinedFiles: page.examined, cataloguedFiles: preparation.files),
                    streamID: stream, indexStatus: AtlasSearchIndexStatus(building: true, reused: false,
                        capturedFiles: page.captured, capturedBytes: page.bytes, skippedFiles: 0, verifiedFiles: 0, verificationBytes: 0)))
                lastPublished = now
            }
            // Cancellation here leaves the last acknowledged Rust step intact.
            // It does not call atlas_cancel, whose epoch would retire the build.
            try check(cancellation)
            do {
                let next = try AtlasIndexBuildPage(preparation.owner.call { try transport.indexStep($0, 1, cancellation) },
                    owner: preparation.handle, layout: preparation.layout, files: preparation.files,
                    discovery: preparation.discovery, command: "index-step")
                guard next.manifest == page.manifest, next.steps == page.steps + 1,
                      next.examined >= page.examined, next.examined <= page.examined + 1,
                      next.captured >= page.captured, next.unavailable >= page.unavailable,
                      next.indexed >= page.indexed, next.uncovered >= page.uncovered,
                      next.bytes >= page.bytes, next.readBytes >= page.readBytes else { throw AtlasIndexWire.invalid() }
                // An intact validated receipt establishes the completed step,
                // even if cancellation raced its return. Save before polling.
                preparation.page = next
            } catch {
                // A canceled/error/malformed foreign response cannot prove how
                // far Rust advanced. Retire it; never resume guessed progress.
                preparing = nil
                throw error
            }
        }
        let page = preparation.page
        guard let captureManifest = page.captureManifest else { throw AtlasIndexWire.invalid() }
        let basis = AtlasIndexedBasis(owner: preparation.handle, manifest: page.manifest, layout: preparation.layout, index: 1,
            captureManifest: captureManifest, files: preparation.files, capturedFiles: page.captured,
            unavailable: page.unavailable, pending: page.pending, sourceBytes: page.bytes, discoveryComplete: preparation.discovery)
        let session = Session(owner: preparation.owner, root: root, grant: grant, basis: basis)
        preparing = nil
        return session
    }
    private func check(_ flag: AtlasSearchCancellation) throws { if flag.isCanceled { throw AtlasSearchError.canceled } }
}

private struct AtlasIndexBuildPage {
    let manifest: UInt64
    let captureManifest: UInt64?
    let running: Bool
    let steps, captured, unavailable, examined, pending, indexed, uncovered: Int
    let bytes, readBytes: UInt64
    init(_ json: String?, owner: UInt64, layout: UInt64, files: Int, discovery: Bool, command: String) throws {
        let w = try AtlasIndexWire.decode(json)
        try w.header(schema: "fcb.atlas-search/1", command: command, owner: owner)
        manifest = try w.number("source_manifest")
        captureManifest = try w.optionalNumber("capture_manifest")
        running = try w.flag("index_build_in_progress")
        captured = try w.count("captured_files", upTo: files)
        unavailable = try w.count("unavailable_files", upTo: files)
        examined = try w.count("examined_files", upTo: files)
        pending = try w.count("pending_files", upTo: files)
        indexed = try w.count("indexed_files", upTo: captured)
        uncovered = try w.count("uncovered_files", upTo: captured)
        bytes = try w.number("indexed_source_bytes"); readBytes = try w.number("initial_source_bytes_read")
        steps = try w.count("build_steps", upTo: 4097)
        let stop = try w.optionalText("stop_reason")
        guard manifest > 0, try w.number("layout_revision") == layout, try w.number("index_generation") == 1,
              try w.text("scope") == "all", try w.number("workspace_files") == UInt64(files),
              captured + unavailable == examined, examined + pending == files, indexed + uncovered == captured,
              bytes <= 32 * 1024 * 1024, readBytes <= 32 * 1024 * 1024, bytes <= readBytes,
              try w.flag("capture_complete") == (!running && pending == 0 && unavailable == 0 && discovery),
              running == (stop == nil) else { throw AtlasIndexWire.invalid() }
        if running {
            guard captureManifest == nil else { throw AtlasIndexWire.invalid() }
        } else {
            let (expected, overflow) = manifest.addingReportingOverflow(1)
            guard !overflow, captureManifest == expected,
                  try w.text("index_kind") == "retained-ephemeral-trigrams",
                  try w.text("source_observation") == "per-file-captures-not-atomic-workspace",
                  try w.flag("discovery_complete") == discovery,
                  ["all-files-examined", "file-limit", "source-byte-limit"].contains(stop ?? "") else { throw AtlasIndexWire.invalid() }
        }
    }
}
