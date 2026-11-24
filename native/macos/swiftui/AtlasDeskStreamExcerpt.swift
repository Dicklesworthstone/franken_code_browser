import Foundation

/// Correlates an accepted stream receipt with the Desk's existing observed
/// passage model. It grants no pathname access and never supplies source bytes.
extension AtlasDeskStreamImport {
    init?(witness: AtlasWorkspaceStreamWitness, revision: UInt64?) {
        let target = witness.target, hit = target.hit
        guard let context = witness.context, let digest = context.extentSHA256,
              context.displayText?.isEmpty == false, let textRange = context.textRange,
              context.originalRange.contains(textRange), textRange.contains(hit.range),
              let captured = try? AtlasDeskObservedRange(start: context.originalRange.start, end: context.originalRange.end),
              let selected = try? AtlasDeskObservedRange(start: textRange.start, end: textRange.end) else { return nil }
        // Preserve the source observation's real consistency; incomplete or
        // unavailable metadata is never relabeled to make retention succeed.
        switch hit.consistency {
        case "unchanged-metadata-not-atomic":
            guard context.complete, hit.finalLength == hit.observedLength else { return nil }
        case "changed-during-read":
            guard context.complete, hit.finalLength != nil else { return nil }
        case "short-read":
            guard !context.complete else { return nil }
        case "metadata-unavailable":
            guard context.complete, hit.finalLength == nil || hit.finalLength == hit.observedLength else { return nil }
        default: return nil
        }
        // Shared decoding may floor a requested prefix to a scalar boundary.
        // Preserve its required context unless this is the absolute source start.
        guard context.originalRange.start == 0 || textRange.start - context.originalRange.start >= 4 else { return nil }
        // EOF is established only by a complete unchanged-metadata observation.
        // Other extents still need the shared decoder's four raw suffix bytes.
        let eofKnown = context.complete && context.originalRange.end == hit.observedLength
            && hit.consistency == "unchanged-metadata-not-atomic"
        guard eofKnown || context.originalRange.end - textRange.end >= 4 else { return nil }
        guard (target.identity.progressive ? revision.map({ $0 > 0 }) == true : revision == nil) else { return nil }
        observation = AtlasDeskObservedImport(originOwner: target.identity.owner, originFile: hit.file,
            originObservation: hit.revision, pathHex: AtlasObservedReaderWire.hex(hit.path),
            encoding: hit.encoding.rawValue, consistency: hit.consistency, extentSHA256: digest,
            capturedRange: captured, selection: selected, observedLength: hit.observedLength,
            finalObservedLength: hit.finalLength)
        family = target.identity.progressive ? 1 : 0
        generation = target.identity.generation; self.revision = revision ?? 0; index = hit.index
        guard isValid else { return nil }
    }
}

extension AtlasReadingDeskModel {
    func canKeepStreamExcerpt(reader: AtlasWorkspaceStreamModel) -> Bool {
        guard canOperate, !hasUnverifiedChanges, supportsStreamExcerpts,
              (state?.annotationCount ?? 0) < 32, let domain = reader.retentionDomain,
              streamDomain == domain, let witness = reader.witnessForRetention else { return false }
        return AtlasDeskStreamImport(witness: witness, revision: reader.report?.summary.revision) != nil
    }

    @discardableResult func keepStreamExcerpt(reader: AtlasWorkspaceStreamModel) -> AtlasDeskStreamImport? {
        guard canKeepStreamExcerpt(reader: reader), let witness = reader.witnessForRetention,
              let domain = reader.retentionDomain else { return nil }
        let revision = reader.report?.summary.revision
        guard let target = AtlasDeskStreamImport(witness: witness, revision: revision) else { return nil }
        keepStreamExcerpt(target, sourceDomain: domain) { work in
            guard reader.retentionDomain == domain else { throw AtlasReadingDeskError.unavailable }
            return try await reader.withRetainedWitness(target: witness.target, revision: revision, work)
        }
        return target
    }

    /// The explicit result ID and original receipt prevent another completed
    /// Keep action from opening an unrelated passage in the same desk.
    func keptStreamExcerpt(_ target: AtlasDeskStreamImport, excluding previous: Set<UInt64>) -> AtlasDeskObservedTarget? {
        guard canOperate, let state, let id = lastKeptObservedPassage, !previous.contains(id),
              let passage = state.observedPassages.first(where: { $0.id == id }),
              let observation = state.observations.first(where: { $0.id == passage.observation }),
              target.observation.matches(observation, passage: passage) else { return nil }
        return AtlasDeskObservedTarget(state: state, passage: passage)
    }
}
