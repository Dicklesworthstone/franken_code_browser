import Foundation

/// The domain identifies the loaded reader registry, not a source, root grant,
/// or process-global native handle. Imports/forks preserve this explicit host
/// capability; independent embedded transports cannot mix numeric handles.
struct AtlasReaderComparisonTransport: Sendable {
    let domain: UUID
    let compare: @Sendable (UInt64, UInt64, UInt64, UInt64, UInt64) -> String?
}

struct AtlasReaderComparisonLimits: Equatable, Sendable {
    var editDistance: UInt64 = 256
    var work: UInt64 = 8 * 1024 * 1024
    func validate() throws {
        guard editDistance <= 512, work <= 64 * 1024 * 1024 else {
            throw AtlasReaderError.invalidInput
        }
    }
}

/// Immutable correspondence bound to two original reader identities and their
/// SHA-256 witnesses. Ranges are original bytes, including empty insertion or
/// deletion sides; decoded text boundaries may differ when a page is opened.
struct AtlasReaderComparisonReport: Sendable {
    struct Range: Equatable, Sendable {
        let start: UInt64
        let end: UInt64
        var length: UInt64 { end - start }
    }
    enum Kind: String, Sendable { case equal, changed, unresolved }
    struct Span: Identifiable, Sendable {
        let id: UInt64
        let kind: Kind
        let before: Range
        let after: Range
        var label: String {
            switch kind {
            case .equal: return "Equal bytes"
            case .unresolved: return "Unresolved range"
            case .changed:
                if before.length == 0 { return "Added bytes" }
                if after.length == 0 { return "Removed bytes" }
                return "Changed bytes"
            }
        }
    }
    let generation: UInt64
    let before: AtlasReaderIdentity
    let after: AtlasReaderIdentity
    let beforeSHA256: String
    let afterSHA256: String
    let quality: String
    let relation: String
    let work: UInt64
    let limits: AtlasReaderComparisonLimits
    let spans: [Span]
    var changes: [Span] { spans.filter { $0.kind != .equal } }
    var summary: String {
        if relation == "identical" { return "The two retained captures contain identical original bytes." }
        let changed = spans.filter { $0.kind == .changed }.count
        let unresolved = spans.filter { $0.kind == .unresolved }.count
        if unresolved > 0 {
            let reason = quality == "work-limit" ? "work budget" : "edit budget"
            return "\(changed) changed ranges; \(unresolved) ranges remain unresolved after the \(reason)."
        }
        return "\(changed) changed ranges in an exact byte comparison."
    }
    func matches(before: AtlasReaderIdentity?, after: AtlasReaderIdentity?) -> Bool {
        before.map { self.before.matches($0) } == true && after.map { self.after.matches($0) } == true
    }

    static func decode(_ json: String?, before: AtlasReaderInfo, after: AtlasReaderInfo,
                       generation: UInt64, limits: AtlasReaderComparisonLimits) throws -> Self {
        try limits.validate()
        guard generation > 0, before.identity.owner != after.identity.owner,
              let beforeDigest = before.captureSHA256, let afterDigest = after.captureSHA256 else {
            throw AtlasReaderError.invalidResponse
        }
        let data = try AtlasReaderWire.admit(json, handle: before.identity.owner)
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let header = try wire.header.identity(handle: before.identity.owner)
            guard wire.header.command == "compare", header.matches(before.identity),
                  AtlasReaderWire.integer(wire.comparison_generation) == generation,
                  let analysisOwner = AtlasReaderWire.integer(wire.comparison_owner), analysisOwner > 0,
                  analysisOwner != before.identity.owner, analysisOwner != after.identity.owner,
                  wire.coordinate_domain == "original-bytes-may-split-text",
                  wire.alignment_semantics == "deterministic-not-identity-proof", !wire.source_reopened,
                  AtlasReaderWire.integer(wire.max_source_bytes) == AtlasReaderLimits.captureBytes,
                  AtlasReaderWire.integer(wire.max_edit_distance) == limits.editDistance,
                  AtlasReaderWire.integer(wire.max_work) == limits.work,
                  let work = AtlasReaderWire.integer(wire.work_units), work <= limits.work,
                  AtlasReaderWire.integer(wire.total_spans) == UInt64(wire.spans.count),
                  wire.spans.count <= Int(2 * limits.editDistance + 5),
                  ["exact", "work-limit", "edit-limit"].contains(wire.comparison_quality),
                  wire.comparison_complete == (wire.comparison_quality == "exact"),
                  ["identical", "different", "undetermined"].contains(wire.relation) else {
                throw AtlasReaderError.invalidResponse
            }
            try wire.before_source.validate(before.identity, digest: beforeDigest)
            try wire.after_source.validate(after.identity, digest: afterDigest)
            var positions: [UInt64] = [0, 0]
            var spans: [Span] = []
            spans.reserveCapacity(wire.spans.count)
            var unresolved = 0
            var editDistance: UInt64 = 0
            for (index, item) in wire.spans.enumerated() {
                let old = try item.before_range.values(), new = try item.after_range.values()
                guard AtlasReaderWire.integer(item.span) == UInt64(index), let kind = Kind(rawValue: item.kind),
                      old.start == positions[0], new.start == positions[1],
                      old.end <= before.identity.capturedBytes, new.end <= after.identity.capturedBytes,
                      old.end != old.start || new.end != new.start else { throw AtlasReaderError.invalidResponse }
                let oldLength = old.end - old.start, newLength = new.end - new.start
                if kind == .equal {
                    guard oldLength == newLength, oldLength > 0 else { throw AtlasReaderError.invalidResponse }
                } else if kind == .unresolved { unresolved += 1 }
                else { editDistance += oldLength + newLength }
                spans.append(Span(id: UInt64(index), kind: kind,
                    before: Range(start: old.start, end: old.end), after: Range(start: new.start, end: new.end)))
                positions = [old.end, new.end]
            }
            guard positions == [before.identity.capturedBytes, after.identity.capturedBytes] else {
                throw AtlasReaderError.invalidResponse
            }
            if wire.comparison_complete {
                guard unresolved == 0, wire.edit_distance.flatMap(AtlasReaderWire.integer) == editDistance,
                      wire.relation == (editDistance == 0 ? "identical" : "different") else {
                    throw AtlasReaderError.invalidResponse
                }
            } else {
                guard unresolved > 0, wire.edit_distance == nil, wire.relation != "identical" else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            return Self(generation: generation, before: before.identity, after: after.identity,
                beforeSHA256: beforeDigest, afterSHA256: afterDigest, quality: wire.comparison_quality,
                relation: wire.relation, work: work, limits: limits, spans: spans)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }

    private struct Wire: Decodable {
        let header: AtlasReaderWire.Header
        let comparison_owner, comparison_generation, comparison_quality, relation: String
        let comparison_complete, source_reopened: Bool
        let coordinate_domain, alignment_semantics: String
        let max_source_bytes, max_edit_distance, max_work, work_units, total_spans: String
        let edit_distance: String?
        let before_source, after_source: Source
        let spans: [Item]
        struct Source: Decodable {
            let owner, file_id, source_revision, captured_bytes, encoding, capture_sha256: String
            let path: AtlasReaderWire.Header.NativePath
            func validate(_ expected: AtlasReaderIdentity, digest: String) throws {
                guard AtlasReaderWire.integer(owner) == expected.owner,
                      AtlasReaderWire.integer(file_id) == expected.file,
                      AtlasReaderWire.integer(source_revision) == expected.revision,
                      AtlasReaderWire.integer(captured_bytes) == expected.capturedBytes,
                      encoding == expected.encoding, capture_sha256 == digest,
                      path.encoding == "unix-bytes", path.display.utf8.count <= 65_536,
                      try AtlasReaderWire.pathBytes(path.hex) == expected.pathBytes else {
                    throw AtlasReaderError.invalidResponse
                }
            }
        }
        struct Item: Decodable {
            let span, kind: String
            let before_range, after_range: AtlasReaderWire.Range
        }
        enum CodingKeys: CodingKey {
            case comparison_owner, comparison_generation, comparison_quality, relation,
                 comparison_complete, source_reopened, coordinate_domain, alignment_semantics,
                 max_source_bytes, max_edit_distance, max_work, work_units, total_spans,
                 edit_distance, before_source, after_source, spans
        }
        init(from decoder: Decoder) throws {
            header = try .init(from: decoder)
            let c = try decoder.container(keyedBy: CodingKeys.self)
            comparison_owner = try c.decode(String.self, forKey: .comparison_owner)
            comparison_generation = try c.decode(String.self, forKey: .comparison_generation)
            comparison_quality = try c.decode(String.self, forKey: .comparison_quality)
            comparison_complete = try c.decode(Bool.self, forKey: .comparison_complete)
            relation = try c.decode(String.self, forKey: .relation)
            source_reopened = try c.decode(Bool.self, forKey: .source_reopened)
            coordinate_domain = try c.decode(String.self, forKey: .coordinate_domain)
            alignment_semantics = try c.decode(String.self, forKey: .alignment_semantics)
            max_source_bytes = try c.decode(String.self, forKey: .max_source_bytes)
            max_edit_distance = try c.decode(String.self, forKey: .max_edit_distance)
            max_work = try c.decode(String.self, forKey: .max_work)
            work_units = try c.decode(String.self, forKey: .work_units)
            total_spans = try c.decode(String.self, forKey: .total_spans)
            edit_distance = try c.decodeIfPresent(String.self, forKey: .edit_distance)
            before_source = try c.decode(Source.self, forKey: .before_source)
            after_source = try c.decode(Source.self, forKey: .after_source)
            spans = try c.decode([Item].self, forKey: .spans)
        }
    }
}
