import Foundation

/// Exact selections cross registries only through a currently leased reader.
/// An extent never enters the complete-capture reader/pane transfer path.
extension AtlasReadingDeskModel {
    func canKeepObserved(reader: AtlasObservedReaderModel) -> Bool {
        canOperate && !hasUnverifiedChanges && supportsObservedPassages && (state?.annotationCount ?? 0) < 32
            && observedDomain == reader.retentionDomain && reader.retentionDomain != nil
            && reader.selectionForRetention != nil
    }

    @discardableResult func keepObserved(reader: AtlasObservedReaderModel) -> Bool {
        guard canKeepObserved(reader: reader), let selected = reader.selectionForRetention,
              let domain = reader.retentionDomain else { return false }
        let observation = selected.observation, identity = observation.identity
        guard let captured = try? AtlasDeskObservedRange(start: observation.capturedRange.start,
                end: observation.capturedRange.end),
              let range = try? AtlasDeskObservedRange(start: selected.originalRange.start,
                end: selected.originalRange.end) else { return false }
        let target = AtlasDeskObservedImport(originOwner: identity.owner, originFile: identity.file,
            originObservation: observation.id, pathHex: AtlasObservedReaderWire.hex(identity.path),
            encoding: observation.request.encoding.rawValue, consistency: observation.consistency,
            extentSHA256: observation.extentSHA256, capturedRange: captured, selection: range,
            observedLength: observation.observedLength, finalObservedLength: observation.finalObservedLength)
        guard target.isValid else { return false }
        keepObserved(target, sourceDomain: domain) { work in
            guard reader.retentionDomain == domain, let current = reader.selectionForRetention,
                  current.id == selected.id, current.observation == observation,
                  current.originalRange == selected.originalRange else {
                throw AtlasReadingDeskError.unavailable
            }
            return try await reader.withRetainedObservation(selectionID: selected.id,
                observation: observation, work)
        }
        return true
    }
}
