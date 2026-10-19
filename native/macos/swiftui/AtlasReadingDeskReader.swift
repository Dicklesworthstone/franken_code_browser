import Foundation

/// A retained desk location and a fresh reader are separate identity domains.
/// This decoder validates their explicit transfer receipt before any page is
/// admitted by the ordinary reader. It never resolves the label as a live path.
enum AtlasDeskReaderTransfer {
    static func readerJSON(_ json: String?, state: AtlasDeskState, pane: AtlasDeskPane,
                           destination: UInt64) throws -> String {
        let data = try AtlasDeskWire.admit(json, handle: state.owner, command: "open-reader")
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let location = try JSONDecoder().decode(AtlasDeskLocation.self, from: data)
            guard wire.model_revision.value == state.revision, wire.last_attempt.value == state.lastAttempt,
                  wire.pane.value == pane.id, location == pane.location,
                  wire.label.utf8.elementsEqual(pane.label.utf8),
                  wire.captured_bytes.value <= AtlasReaderLimits.captureBytes,
                  wire.captured_bytes.value <= state.retainedBytes,
                  location.offset <= wire.captured_bytes.value,
                  location.selection == nil || location.selection!.end <= wire.captured_bytes.value,
                  wire.additional_source_bytes_read.value == 0, !wire.native_presented, !wire.source_reopened,
                  state.panes.contains(where: { $0.id == pane.id && $0.location == pane.location
                    && $0.label.utf8.elementsEqual(pane.label.utf8) }),
                  let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let reader = object["reader"] as? [String: Any] else {
                throw AtlasReadingDeskError.invalidResponse
            }
            let encoded = try JSONSerialization.data(withJSONObject: reader, options: [.sortedKeys])
            guard let nested = String(data: encoded, encoding: .utf8) else { throw AtlasReadingDeskError.invalidResponse }
            let info = try AtlasReaderInfo.decodeRetainedInitialization(nested, handle: destination, label: pane.label)
            guard info.identity.capturedBytes == wire.captured_bytes.value else { throw AtlasReadingDeskError.invalidResponse }
            return nested
        } catch let error as AtlasReadingDeskError { throw error }
        catch { throw AtlasReadingDeskError.invalidResponse }
    }
    private struct Wire: Decodable {
        let model_revision, last_attempt, pane, captured_bytes, additional_source_bytes_read: AtlasDeskNumber
        let label: String
        let native_presented, source_reopened: Bool
    }
}

/// One independent reader of the original retained pane. Navigation does not
/// mutate the desk until an explicit user action validates the original source
/// association again. Closing/replacing the desk leaves this capture readable.
@MainActor final class AtlasDeskReaderCapture: Identifiable {
    let id = UUID()
    let reader: AtlasPagedReaderModel
    let owner: UInt64
    let pane: AtlasDeskPane
    let identity: AtlasReaderIdentity
    init(reader: AtlasPagedReaderModel, owner: UInt64, pane: AtlasDeskPane, identity: AtlasReaderIdentity) {
        self.reader = reader; self.owner = owner; self.pane = pane; self.identity = identity
    }

    func canUpdate(_ desk: AtlasReadingDeskModel) -> Bool {
        guard desk.canOperate, !reader.busy, let current = reader.page,
              current.identity.matches(identity), desk.state?.owner == owner,
              let original = desk.state?.panes.first(where: { $0.id == pane.id }) else { return false }
        return original.location.file == pane.location.file && original.location.revision == pane.location.revision
    }

    /// Only Rust-authored original byte ranges may return to the desk. A native
    /// glyph/text selection is not silently interpreted as source bytes.
    @discardableResult func keepCurrentPage(in desk: AtlasReadingDeskModel) -> Bool {
        guard canUpdate(desk), let page = reader.page else { return false }
        let selected: (UInt64, UInt64)
        if reader.findIsCurrent, let hit = page.selection, reader.matchHex != nil {
            selected = (hit.target.start, hit.target.end)
        } else if reader.outlineIsCurrent, let symbol = page.symbolSelection, reader.symbolHex != nil {
            let range = symbol.target.symbol.selectedRange
            selected = (range.start, range.end)
        } else { selected = (page.start, page.end) }
        return desk.updateRetainedLocation(owner: owner, pane: pane.id, file: pane.location.file,
            sourceRevision: pane.location.revision, start: selected.0, end: selected.1)
    }
}

extension AtlasReadingDeskModel {
    /// The destination has no source/root grant and can never acquire one from
    /// the checkpoint's label. Every failure closes even a late-initialized
    /// destination; the exporting desk's operation holds its own lease to drain.
    func openReader(pane paneID: UInt64, transport: AtlasReaderTransport,
        search: AtlasReaderSearchTransport? = nil, outline: AtlasReaderOutlineTransport? = nil,
        document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasDeskReaderCapture {
        guard let domain = readerDomain, transport.comparison?.domain == domain else {
            throw AtlasReadingDeskError.unavailable
        }
        let reader = AtlasPagedReaderModel(transport: transport, search: search, outline: outline, document: document)
        do {
            return try await withRetainedPane(paneID, destinationDomain: domain) { state, pane, initialize in
                try await reader.openRetained(label: pane.label, offset: pane.location.offset, registryDomain: domain) { destination in
                    try AtlasDeskReaderTransfer.readerJSON(initialize(destination), state: state, pane: pane, destination: destination)
                }
                guard let page = reader.page else { throw AtlasReadingDeskError.invalidResponse }
                return AtlasDeskReaderCapture(reader: reader, owner: state.owner, pane: pane, identity: page.identity)
            }
        } catch { reader.close(); throw error }
    }

    /// Keep the exact original range the user saw, including a retained search
    /// or supplied capture. Source replacement while the desk starts revokes
    /// this intent; it cannot silently import the replacement capture instead.
    func keep(reader: AtlasPagedReaderModel) {
        guard canOperate, reader.canPin, let original = reader.page,
              let expectedDomain = readerDomain, reader.retainedRegistryDomain == expectedDomain else { return }
        keepCapture(start: original.start, end: original.end, sourceDomain: expectedDomain) { work in
            guard let current = reader.page, current.identity.matches(original.identity), reader.canPin,
                  reader.retainedRegistryDomain == expectedDomain else {
                throw AtlasReadingDeskError.unavailable
            }
            return try await reader.withRetainedReader(work)
        }
    }
}
