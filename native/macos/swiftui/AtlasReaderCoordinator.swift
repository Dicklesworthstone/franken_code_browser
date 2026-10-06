import Foundation
import Dispatch

/// Injected only at the C-marshaling boundary. The production implementation
/// invokes the retained Rust reader; tests exercise this coordinator and the
/// production bounded I/O queue without linking Apple frameworks or Rust.
struct AtlasReaderTransport: Sendable {
    let create: @Sendable () -> UInt64
    let open: @Sendable (UInt64, String, UInt64) -> String?
    let window: @Sendable (UInt64, UInt64, UInt64) -> String?
    let lines: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
    let cancel: @Sendable (UInt64) -> Bool
    let close: @Sendable (UInt64) -> Bool
    let retirementFailed: @Sendable () -> Void
}

/// This reference is retained, never operated on, by background work. It may
/// hold the native security-scoped root grant for the entire capture lifetime.
private final class AtlasReaderAccessLease: @unchecked Sendable {
    let reference: AnyObject?
    init(_ reference: AnyObject?) { self.reference = reference }
}

/// All fields are immutable; mutable Rust state is protected by its registry.
/// Foreign calls retain this object until return. Its last Swift owner queues
/// retirement rather than deallocating a large Rust capture on the main actor.
private final class AtlasReaderSession: @unchecked Sendable {
    private static let retirement = DispatchQueue(label: "dev.frankencode.browser.reader-retirement", qos: .utility)
    let handle: UInt64
    let transport: AtlasReaderTransport
    private let access: AtlasReaderAccessLease

    init(transport: AtlasReaderTransport, accessLease: AnyObject?) throws {
        let handle = transport.create()
        guard handle != 0 else { throw AtlasReaderError.unavailable }
        self.handle = handle
        self.transport = transport
        access = AtlasReaderAccessLease(accessLease)
    }

    func cancel() { _ = transport.cancel(handle) }

    deinit {
        let handle = handle, transport = transport, access = access
        Self.retirement.async {
            withExtendedLifetime(access) {
                // The registry uses try_lock and can briefly refuse a close.
                // At most eight reader handles exist, so this retirement queue
                // cannot retain an unbounded number of source captures. Retry
                // contention for a bounded interval; never claim failed cleanup.
                for attempt in 0..<8 {
                    if transport.close(handle) { return }
                    if attempt < 7 { Thread.sleep(forTimeInterval: 0.001) }
                }
                transport.retirementFailed()
            }
        }
    }
}

/// One retained capture and the existing one-active/one-newest native I/O lane.
/// A replacement or close invalidates delivery immediately, but neither the
/// source handle nor its grant can retire before the active foreign call ends.
@MainActor final class AtlasReaderCoordinator {
    private let transport: AtlasReaderTransport
    private let searchTransport: AtlasReaderSearchTransport?
    private let outlineTransport: AtlasReaderOutlineTransport?
    private var outlineInventory: AtlasOutlineInventory?
    private var acceptedOutline: AtlasOutlinePage?
    private var lastOutlineGeneration: UInt64 = 0
    private var acceptedFind: AtlasReaderFindReport?
    private var lastFindGeneration: UInt64 = 0
    private let io = AtlasProjectIO()
    private var session: AtlasReaderSession?
    private var info: AtlasReaderInfo?
    private var cancellation: AtlasSearchCancellation?
    private var operation = UUID()

    init(transport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil,
         outline: AtlasReaderOutlineTransport? = nil) {
        self.transport = transport
        self.searchTransport = search
        self.outlineTransport = outline
    }
    var supportsFind: Bool { searchTransport != nil }
    var supportsOutline: Bool { outlineTransport != nil }

    deinit {
        cancellation?.cancel()
        session?.cancel()
    }

    func open(input: AtlasReaderInput, accessLease: AnyObject? = nil) async throws -> AtlasReaderPage {
        // Empty-handle admission is small and performs no filesystem work.
        // A refused replacement does not destroy an already admitted capture.
        let next = try AtlasReaderSession(transport: transport, accessLease: accessLease)
        close()
        session = next
        let ticket = UUID()
        operation = ticket
        let flag = AtlasSearchCancellation()
        cancellation = flag
        do {
            let result = try await io.perform(cancellation: flag, keepingAlive: [next]) {
                let info = try AtlasReaderInfo.decode(
                    next.transport.open(next.handle, input.fullPath, AtlasReaderLimits.captureBytes),
                    handle: next.handle, path: input.fullPath)
                guard !flag.isCanceled else { throw AtlasReaderError.canceled }
                let request = AtlasReaderRequest.firstPage
                let page = try AtlasReaderPage.decode(
                    next.transport.window(next.handle, 0, request.byteLimit), info: info, request: request)
                return (info, page)
            }
            guard session === next, operation == ticket, !flag.isCanceled else { throw AtlasReaderError.canceled }
            info = result.0
            cancellation = nil
            return result.1
        } catch {
            if session === next, operation == ticket { close() }
            throw Self.readerError(error)
        }
    }

    func read(_ request: AtlasReaderRequest) async throws -> AtlasReaderPage {
        guard let session, let info else { throw AtlasReaderError.unavailable }
        // Reject invalid navigation before canceling an admitted operation.
        try request.validate(capturedBytes: info.identity.capturedBytes)
        try validateSelection(request)
        let outline = outlineTransport
        let search = searchTransport
        cancel()
        let ticket = UUID()
        operation = ticket
        let flag = AtlasSearchCancellation()
        cancellation = flag
        do {
            let page = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                let json: String?
                switch request {
                case .window(let offset, let bytes):
                    json = session.transport.window(session.handle, offset, bytes)
                case .lines(let first, let count, let bytes):
                    json = session.transport.lines(session.handle, first, count, bytes)
                case .hit(let target):
                    guard let search else { throw AtlasReaderError.unavailable }
                    json = search.hit(session.handle, target.generation, target.index, AtlasReaderHitTarget.contextBytes)
                case .symbol(let target):
                    guard let outline else { throw AtlasReaderError.unavailable }
                    json = outline.symbol(session.handle, target.generation, target.symbol.id, AtlasOutlineTarget.contextBytes)
                }
                return try AtlasReaderPage.decode(json, info: info, request: request)
            }
            guard self.session === session, operation == ticket, !flag.isCanceled else { throw AtlasReaderError.canceled }
            // A host may revoke rows while foreign work is draining without
            // canceling unrelated source work. Recheck authority at delivery.
            try validateSelection(request)
            cancellation = nil
            return page
        } catch {
            if self.session === session, operation == ticket { cancellation = nil }
            throw Self.readerError(error)
        }
    }

    private func validateSelection(_ request: AtlasReaderRequest) throws {
        if case .hit(let target) = request {
            guard searchTransport != nil, let acceptedFind,
                  acceptedFind.target(at: Int(target.index)) == target else { throw AtlasReaderError.invalidInput }
        }
        if case .symbol(let target) = request {
            guard outlineTransport != nil, let acceptedOutline,
                  acceptedOutline.target(id: target.symbol.id) == target else {
                throw AtlasReaderError.invalidInput
            }
        }
    }

    /// Search the retained source, not the current disk file or only the visible
    /// page. Attempts consume generations even when canceled or refused later.
    func find(_ needle: String) async throws -> AtlasReaderFindReport {
        guard let session, let info, let search = searchTransport else { throw AtlasReaderError.unavailable }
        try AtlasReaderFindReport.validateNeedle(needle)
        let (generation, overflow) = lastFindGeneration.addingReportingOverflow(1)
        guard !overflow else { throw AtlasReaderError.unavailable }
        lastFindGeneration = generation
        acceptedFind = nil
        cancel()
        let ticket = UUID()
        operation = ticket
        let flag = AtlasSearchCancellation()
        cancellation = flag
        do {
            let report = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                try AtlasReaderFindReport.decode(
                    search.find(session.handle, generation, needle, AtlasReaderFindReport.maxHits, info.identity.capturedBytes),
                    info: info, generation: generation, needle: needle)
            }
            guard self.session === session, operation == ticket, !flag.isCanceled else { throw AtlasReaderError.canceled }
            acceptedFind = report
            cancellation = nil
            return report
        } catch {
            if self.session === session, operation == ticket { cancellation = nil }
            throw Self.readerError(error)
        }
    }

    /// Build the shared extractor's bounded inventory over this same capture.
    /// Search and outline use independent counters, even when both equal one.
    func prepareOutline(language: AtlasOutlineLanguage = .automatic) async throws -> AtlasOutlinePage {
        guard let session, let info, let outline = outlineTransport else { throw AtlasReaderError.unavailable }
        let (generation, overflow) = lastOutlineGeneration.addingReportingOverflow(1)
        guard !overflow else { throw AtlasReaderError.unavailable }
        lastOutlineGeneration = generation
        resetOutline()
        cancel()
        let ticket = UUID()
        operation = ticket
        let flag = AtlasSearchCancellation()
        cancellation = flag
        do {
            let page = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                try AtlasOutlinePage.decode(
                    outline.prepare(session.handle, generation, language.engineName, AtlasOutlinePage.maxItems),
                    info: info, generation: generation, request: .initial, preparing: true, language: language)
            }
            guard self.session === session, operation == ticket, !flag.isCanceled else { throw AtlasReaderError.canceled }
            outlineInventory = page.inventory
            acceptedOutline = page
            cancellation = nil
            return page
        } catch {
            if self.session === session, operation == ticket { cancellation = nil }
            throw Self.readerError(error)
        }
    }

    /// Page/filter already-retained symbols. No additional source capture or
    /// extraction. Failed work cannot authorize rows for the attempted filter.
    func outlineSymbols(_ request: AtlasOutlinePageRequest) async throws -> AtlasOutlinePage {
        try request.validate()
        guard let session, let info, let outline = outlineTransport,
              let inventory = outlineInventory else { throw AtlasReaderError.unavailable }
        acceptedOutline = nil
        cancel()
        let ticket = UUID()
        operation = ticket
        let flag = AtlasSearchCancellation()
        cancellation = flag
        do {
            let page = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                try AtlasOutlinePage.decode(
                    outline.symbols(session.handle, inventory.generation, request.needle,
                        request.mode.code, request.start, request.limit),
                    info: info, generation: inventory.generation, request: request, inventory: inventory)
            }
            guard self.session === session, operation == ticket, !flag.isCanceled,
                  outlineInventory?.matches(inventory) == true else { throw AtlasReaderError.canceled }
            acceptedOutline = page
            cancellation = nil
            return page
        } catch {
            if self.session === session, operation == ticket { cancellation = nil }
            throw Self.readerError(error)
        }
    }

    /// Revoke native rows, not source/search state or the bounded Rust index.
    func revokeOutlinePage() { acceptedOutline = nil }
    func resetOutline() { acceptedOutline = nil; outlineInventory = nil }

    /// Revoke native activation authority without evicting captured source or
    /// interrupting unrelated byte/line navigation. The host cancels active
    /// query work separately when editing its query context.
    func clearFind() { acceptedFind = nil }

    func cancel() {
        operation = UUID()
        cancellation?.cancel()
        cancellation = nil
        session?.cancel()
        io.cancel()
    }

    func close() {
        cancel()
        info = nil
        acceptedFind = nil
        lastFindGeneration = 0
        resetOutline()
        lastOutlineGeneration = 0
        session = nil
    }

    private static func readerError(_ error: Error) -> AtlasReaderError {
        if let error = error as? AtlasReaderError { return error }
        if error is CancellationError { return .canceled }
        if let error = error as? AtlasProjectIOError, case .canceled = error { return .canceled }
        return .unavailable
    }
}
