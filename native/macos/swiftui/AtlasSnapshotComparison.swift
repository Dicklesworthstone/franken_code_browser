import Foundation

enum AtlasSnapshotComparisonError: Error, Sendable {
    case unavailable, invalidInput, invalidResponse, canceled, engine(String)
    var message: String {
        switch self {
        case .unavailable: return "The saved comparison could not admit this operation. Close an unused comparison and try again."
        case .invalidInput: return "Choose two saved FCBS archives and a current captured comparison entry."
        case .invalidResponse: return "The saved comparison response could not be verified. The previous accepted comparison remains available."
        case .canceled: return "Saved comparison work was canceled."
        case .engine(let code): return "The saved comparison operation was refused (\(code))."
        }
    }
}

enum AtlasSnapshotComparisonSide: String, CaseIterable, Identifiable, Sendable {
    case before, after
    var id: String { rawValue }
    var label: String { self == .before ? "Before" : "After" }
    var code: UInt64 { self == .before ? 0 : 1 }
}
enum AtlasSnapshotComparisonKind: String, CaseIterable, Identifiable, Sendable {
    case unchanged, changed, added, removed, onlyBefore = "only-before", onlyAfter = "only-after", unavailable
    var id: String { rawValue }
    var label: String {
        switch self {
        case .unchanged: "Unchanged"
        case .changed: "Changed"
        case .added: "Added in saved scope"
        case .removed: "Removed from saved scope"
        case .onlyBefore: "Only before · scope uncertain"
        case .onlyAfter: "Only after · scope uncertain"
        case .unavailable: "Capture unavailable"
        }
    }
    var isUncertain: Bool { self == .onlyBefore || self == .onlyAfter || self == .unavailable }
}
enum AtlasSnapshotComparisonFilter: String, CaseIterable, Identifiable, Sendable {
    case all, differences, unchanged, changed, added, removed
    case onlyBefore = "only-before", onlyAfter = "only-after", unavailable
    var id: String { rawValue }
    var label: String {
        switch self {
        case .all: "All paths"
        case .differences: "Differences and uncertainty"
        default: AtlasSnapshotComparisonKind(rawValue: rawValue)!.label
        }
    }
    var code: UInt64 {
        switch self {
        case .all: 0; case .differences: 1; case .unchanged: 2; case .changed: 3
        case .added: 4; case .removed: 5; case .onlyBefore: 6; case .onlyAfter: 7; case .unavailable: 8
        }
    }
    func includes(_ kind: AtlasSnapshotComparisonKind) -> Bool {
        self == .all || (self == .differences ? kind != .unchanged : rawValue == kind.rawValue)
    }
}

/// Two explicitly selected archive descriptors belong to one private owner.
/// Paths are admitted only by open; paging and activation never reopen them.
struct AtlasSnapshotComparisonTransport: Sendable {
    let create: @Sendable () -> UInt64
    let open: @Sendable (UInt64, String, String, AtlasSearchCancellation) throws -> String?
    let info: @Sendable (UInt64) throws -> String?
    let begin: @Sendable (UInt64, UInt64, AtlasSearchCancellation) throws -> String?
    let step: @Sendable (UInt64, UInt64, UInt64, AtlasSearchCancellation) throws -> String?
    let page: @Sendable (UInt64, UInt64, AtlasSnapshotComparisonFilter, UInt64, UInt64) throws -> String?
    let openReader: @Sendable (UInt64, UInt64, UInt64, UInt64, AtlasSnapshotComparisonSide) throws -> String?
    let cancel: @Sendable (UInt64) -> Bool
    let close: @Sendable (UInt64) -> Bool
    let retirementFailed: @Sendable () -> Void
    let readerDomain: UUID?
}

struct AtlasSnapshotComparisonPath: Sendable, Equatable {
    let bytes: [UInt8]
    let display: String
    var hex: String { bytes.map { String(format: "%02x", $0) }.joined() }
    init(_ wire: AtlasSnapshotComparisonWire, absolute: Bool) throws {
        let hex = try wire.text("hex")
        display = try wire.text("display")
        guard try wire.text("encoding") == "unix-bytes",
              !hex.isEmpty, hex.utf8.count <= (absolute ? 8192 : 4096),
              hex.utf8.count.isMultiple(of: 2), display.utf8.count <= 16_384 else {
            throw AtlasSnapshotComparisonError.invalidResponse
        }
        bytes = try AtlasReaderWire.pathBytes(hex)
        guard !bytes.isEmpty, !bytes.contains(0),
              absolute ? bytes.first == 47 : Self.validMember(bytes) else {
            throw AtlasSnapshotComparisonError.invalidResponse
        }
    }
    static func validMember(_ bytes: [UInt8]) -> Bool {
        !bytes.isEmpty && bytes.count <= 2048 && bytes.first != 47 && !bytes.contains(0)
            && !bytes.split(separator: 47, omittingEmptySubsequences: false).contains {
                $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
            }
    }
    static func validArchive(_ path: String) -> Bool {
        path.hasPrefix("/") && path.utf8.count <= 4096 && !path.utf8.contains(0)
    }
    static func == (a: Self, b: Self) -> Bool {
        a.bytes == b.bytes && a.display.utf8.elementsEqual(b.display.utf8)
    }
}

struct AtlasSnapshotComparisonArchive: Sendable, Equatable {
    let digest: String
    let path: AtlasSnapshotComparisonPath
    let policy: String
    let members, captured, sourceBytes, archiveBytes, initialBytes, initialCalls: UInt64
    let discoveryComplete: Bool
    fileprivate init(_ wire: AtlasSnapshotComparisonWire) throws {
        digest = try wire.text("digest")
        path = try AtlasSnapshotComparisonPath(wire.object("path"), absolute: true)
        policy = try wire.text("policy")
        members = try wire.number("members_total"); captured = try wire.number("captured_members")
        sourceBytes = try wire.number("source_bytes"); archiveBytes = try wire.number("archive_bytes")
        initialBytes = try wire.number("initial_archive_bytes_read")
        initialCalls = try wire.number("initial_archive_read_calls")
        discoveryComplete = try wire.flag("discovery_complete")
        guard AtlasSnapshotComparisonWire.digest(digest), !policy.isEmpty, policy.utf8.count <= 128,
              policy.utf8.allSatisfy({ (33...126).contains($0) }),
              members <= 65_536, captured <= members, sourceBytes <= 64 * 1024 * 1024,
              archiveBytes > 0, archiveBytes <= 80 * 1024 * 1024, sourceBytes <= archiveBytes,
              initialBytes == archiveBytes, initialCalls > 0, initialCalls <= 1_048_576,
              captured > 0 || sourceBytes == 0 else { throw AtlasSnapshotComparisonError.invalidResponse }
    }
    static func == (a: Self, b: Self) -> Bool {
        a.digest == b.digest && a.path == b.path && a.policy.utf8.elementsEqual(b.policy.utf8)
            && a.members == b.members && a.captured == b.captured && a.sourceBytes == b.sourceBytes
            && a.archiveBytes == b.archiveBytes && a.initialBytes == b.initialBytes
            && a.initialCalls == b.initialCalls && a.discoveryComplete == b.discoveryComplete
    }
}
struct AtlasSnapshotComparisonIdentity: Sendable, Equatable {
    let owner: UInt64
    let before, after: AtlasSnapshotComparisonArchive
    var policiesMatch: Bool { before.policy.utf8.elementsEqual(after.policy.utf8) }
    var membershipComparable: Bool { policiesMatch && before.discoveryComplete && after.discoveryComplete }
    func archive(_ side: AtlasSnapshotComparisonSide) -> AtlasSnapshotComparisonArchive {
        side == .before ? before : after
    }
}
struct AtlasSnapshotComparisonIOCounts: Sendable, Equatable {
    let beforeBytes, beforeCalls, afterBytes, afterCalls: UInt64
    func follows(_ old: Self) -> Bool {
        beforeBytes >= old.beforeBytes && beforeCalls >= old.beforeCalls
            && afterBytes >= old.afterBytes && afterCalls >= old.afterCalls
    }
}
struct AtlasSnapshotComparisonInfo: Sendable {
    let identity: AtlasSnapshotComparisonIdentity
    let io: AtlasSnapshotComparisonIOCounts
    let lastAttempt: UInt64
    let acceptedGeneration, pendingGeneration: UInt64?

    static func decode(_ json: String?, owner: UInt64, before: [UInt8], after: [UInt8],
        fresh: Bool = false) throws -> Self {
        do {
            let w = try AtlasSnapshotComparisonWire.admit(json, owner: owner, command: "info")
            let result = try w.info()
            guard result.identity.before.path.bytes == before, result.identity.after.path.bytes == after,
                  !fresh || (result.lastAttempt == 0 && result.acceptedGeneration == nil
                    && result.pendingGeneration == nil && result.io.beforeBytes == 0
                    && result.io.afterBytes == 0 && result.io.beforeCalls == 0 && result.io.afterCalls == 0) else {
                throw AtlasSnapshotComparisonError.invalidResponse
            }
            return result
        } catch let error as AtlasSnapshotComparisonError { throw error }
        catch { throw AtlasSnapshotComparisonError.invalidResponse }
    }
}

/// Strict canonical integers and explicit nulls keep row authority independent
/// of JSON number rounding, display strings and optional-field defaults.
struct AtlasSnapshotComparisonWire: Decodable {
    private let values: [String: Value]
    indirect enum Value: Decodable {
        case text(String), bool(Bool), object([String: Value]), array([Value]), null
        init(from decoder: Decoder) throws {
            let c = try decoder.singleValueContainer()
            if c.decodeNil() { self = .null }
            else if let text = try? c.decode(String.self) { self = .text(text) }
            else if let flag = try? c.decode(Bool.self) { self = .bool(flag) }
            else if let object = try? c.decode([String: Value].self) { self = .object(object) }
            else if let array = try? c.decode([Value].self) { self = .array(array) }
            else { throw AtlasSnapshotComparisonError.invalidResponse }
        }
    }
    init(from decoder: Decoder) throws {
        values = try decoder.singleValueContainer().decode([String: Value].self)
    }
    private init(_ values: [String: Value]) { self.values = values }
    func text(_ key: String) throws -> String {
        guard case .text(let text) = values[key] else { throw AtlasSnapshotComparisonError.invalidResponse }
        return text
    }
    func number(_ key: String) throws -> UInt64 {
        guard let result = AtlasReaderWire.integer(try text(key)) else { throw AtlasSnapshotComparisonError.invalidResponse }
        return result
    }
    func flag(_ key: String) throws -> Bool {
        guard case .bool(let flag) = values[key] else { throw AtlasSnapshotComparisonError.invalidResponse }
        return flag
    }
    func isNull(_ key: String) -> Bool { if case .null = values[key] { return true }; return false }
    func optionalNumber(_ key: String) throws -> UInt64? { isNull(key) ? nil : try number(key) }
    func optionalText(_ key: String) throws -> String? { isNull(key) ? nil : try text(key) }
    func object(_ key: String) throws -> Self {
        guard case .object(let object) = values[key] else { throw AtlasSnapshotComparisonError.invalidResponse }
        return Self(object)
    }
    func array(_ key: String) throws -> [Self] {
        guard case .array(let array) = values[key] else { throw AtlasSnapshotComparisonError.invalidResponse }
        return try array.map {
            guard case .object(let object) = $0 else { throw AtlasSnapshotComparisonError.invalidResponse }
            return Self(object)
        }
    }
    static let maximumResponseBytes = 4 * 1024 * 1024
    static func digest(_ text: String) -> Bool {
        text.utf8.count == 64 && text.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    static func admit(_ json: String?, owner: UInt64, command: String) throws -> Self {
        guard let json, !json.isEmpty, json.utf8.count <= maximumResponseBytes else {
            throw AtlasSnapshotComparisonError.unavailable
        }
        do {
            let wire = try JSONDecoder().decode(Self.self, from: Data(json.utf8))
            guard owner > 0, try wire.text("schema") == "fcb.snapshot-comparison/1",
                  try wire.number("owner") == owner else { throw AtlasSnapshotComparisonError.invalidResponse }
            if try wire.text("status") == "error" {
                let code = try wire.object("error").text("code")
                guard !code.isEmpty, code.utf8.count <= 128,
                      code.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || $0 == 95 }) else {
                    throw AtlasSnapshotComparisonError.invalidResponse
                }
                throw AtlasSnapshotComparisonError.engine(code)
            }
            guard try wire.text("status") == "ok", try wire.text("command") == command,
                  try !wire.flag("live_source_reopened"), try wire.number("live_source_bytes_read") == 0,
                  try !wire.flag("history_inferred"), try !wire.flag("renames_inferred"),
                  try wire.text("alignment") == "native-path-pair-not-logical-file-continuity",
                  try !wire.flag("native_presented"), try wire.number("max_members_per_archive") == 65_536,
                  try wire.number("max_path_bytes") == 2048, try wire.number("max_source_file_bytes") == 1024 * 1024,
                  try wire.number("max_archive_bytes") == 80 * 1024 * 1024,
                  try wire.number("max_retained_entries") == 131_072,
                  try wire.number("max_response_bytes") == UInt64(maximumResponseBytes) else {
                throw AtlasSnapshotComparisonError.invalidResponse
            }
            return wire
        } catch let error as AtlasSnapshotComparisonError { throw error }
        catch { throw AtlasSnapshotComparisonError.invalidResponse }
    }
    func info() throws -> AtlasSnapshotComparisonInfo {
        let before = try object("before_archive"), after = try object("after_archive")
        let identity = try AtlasSnapshotComparisonIdentity(owner: number("owner"),
            before: AtlasSnapshotComparisonArchive(before), after: AtlasSnapshotComparisonArchive(after))
        let io = try AtlasSnapshotComparisonIOCounts(beforeBytes: before.number("member_bytes_read"),
            beforeCalls: before.number("member_read_calls"), afterBytes: after.number("member_bytes_read"),
            afterCalls: after.number("member_read_calls"))
        let last = try number("last_comparison_attempt")
        let accepted = try optionalNumber("accepted_comparison_generation")
        let pending = try optionalNumber("pending_comparison_generation")
        guard accepted.map({ $0 > 0 && $0 <= last }) != false,
              pending.map({ $0 > 0 && $0 == last && $0 > (accepted ?? 0) }) != false else {
            throw AtlasSnapshotComparisonError.invalidResponse
        }
        return AtlasSnapshotComparisonInfo(identity: identity, io: io, lastAttempt: last,
            acceptedGeneration: accepted, pendingGeneration: pending)
    }
}

struct AtlasSnapshotComparisonMember: Sendable, Equatable {
    let ordinal, file, revision, observedBytes: UInt64
    let captured: Bool
    let bytes: UInt64?
    let digest: String?
    let unavailableReason: String?
    fileprivate init(_ wire: AtlasSnapshotComparisonWire, side: AtlasSnapshotComparisonSide,
        archive: AtlasSnapshotComparisonArchive) throws {
        ordinal = try wire.number("member"); file = try wire.number("file_id")
        revision = try wire.number("source_revision"); observedBytes = try wire.number("observed_bytes")
        captured = try wire.flag("captured"); bytes = try wire.optionalNumber("captured_bytes")
        digest = try wire.optionalText("source_digest")
        unavailableReason = try wire.optionalText("unavailable_reason")
        guard ordinal < archive.members, file == ordinal + (side == .before ? 1 : 65_537),
              revision == file else { throw AtlasSnapshotComparisonError.invalidResponse }
        if captured {
            guard let bytes, bytes <= 1024 * 1024, bytes == observedBytes, bytes <= archive.sourceBytes,
                  let digest, AtlasSnapshotComparisonWire.digest(digest), unavailableReason == nil else {
                throw AtlasSnapshotComparisonError.invalidResponse
            }
        } else {
            guard bytes == nil, digest == nil, let reason = unavailableReason,
                  !reason.isEmpty, reason.utf8.count <= 96,
                  reason.utf8.allSatisfy({ (65...90).contains($0) || (48...57).contains($0) || $0 == 95 }) else {
                throw AtlasSnapshotComparisonError.invalidResponse
            }
        }
    }
}

struct AtlasSnapshotComparisonRow: Identifiable, Sendable, Equatable {
    let id: UInt64
    let path: AtlasSnapshotComparisonPath
    let kind: AtlasSnapshotComparisonKind
    let before, after: AtlasSnapshotComparisonMember?
    var label: String { kind.label }
    func member(_ side: AtlasSnapshotComparisonSide) -> AtlasSnapshotComparisonMember? { side == .before ? before : after }
    func canOpen(_ side: AtlasSnapshotComparisonSide) -> Bool {
        guard let member = member(side), member.captured, let bytes = member.bytes else { return false }
        return bytes <= AtlasReaderLimits.transferBytes
    }
    fileprivate init(_ wire: AtlasSnapshotComparisonWire, identity: AtlasSnapshotComparisonIdentity) throws {
        id = try wire.number("entry_id")
        path = try AtlasSnapshotComparisonPath(wire.object("path"), absolute: false)
        guard let kind = AtlasSnapshotComparisonKind(rawValue: try wire.text("kind")), id > 0,
              id <= identity.before.members + identity.after.members else {
            throw AtlasSnapshotComparisonError.invalidResponse
        }
        self.kind = kind
        before = wire.isNull("before") ? nil : try AtlasSnapshotComparisonMember(wire.object("before"),
            side: .before, archive: identity.before)
        after = wire.isNull("after") ? nil : try AtlasSnapshotComparisonMember(wire.object("after"),
            side: .after, archive: identity.after)
        let expected: AtlasSnapshotComparisonKind
        switch (before, after) {
        case (nil, nil): throw AtlasSnapshotComparisonError.invalidResponse
        case (.some(let a), .some(let b)):
            if !a.captured || !b.captured { expected = .unavailable }
            else { expected = a.bytes == b.bytes && a.digest == b.digest ? .unchanged : .changed }
        case (.some(let a), nil):
            expected = !a.captured ? .unavailable
                : identity.policiesMatch && identity.after.discoveryComplete ? .removed : .onlyBefore
        case (nil, .some(let b)):
            expected = !b.captured ? .unavailable
                : identity.policiesMatch && identity.before.discoveryComplete ? .added : .onlyAfter
        }
        guard kind == expected else { throw AtlasSnapshotComparisonError.invalidResponse }
    }
}

struct AtlasSnapshotComparisonProgress: Sendable, Equatable {
    let generation, upperBound, compared, retained, steps, lastStepEntries: UInt64
    let unchanged, changed, added, removed, onlyBefore, onlyAfter, unavailable: UInt64
    let knownDifferences, uncertain: UInt64
    let building, scanComplete, complete, policiesMatch, membershipComparable: Bool
    var summary: String {
        let prefix = building ? "Comparing saved paths: \(compared) processed, at most \(upperBound) total. "
            : "\(compared) saved paths compared. "
        return prefix + "\(changed) changed, \(added) added, \(removed) removed, \(unchanged) unchanged; \(uncertain) uncertain. "
            + (membershipComparable ? "" : "Capture policies or discovery coverage differ; missing paths may be uncertain. ")
            + "Path matches do not prove logical file continuity, renames, or Git history."
    }
    func count(for filter: AtlasSnapshotComparisonFilter) -> UInt64 {
        switch filter {
        case .all: retained; case .differences: retained - unchanged
        case .unchanged: unchanged; case .changed: changed; case .added: added; case .removed: removed
        case .onlyBefore: onlyBefore; case .onlyAfter: onlyAfter; case .unavailable: unavailable
        }
    }
    fileprivate init(_ wire: AtlasSnapshotComparisonWire, info: AtlasSnapshotComparisonInfo,
        generation expected: UInt64) throws {
        generation = try wire.number("comparison_generation")
        upperBound = try wire.number("paths_upper_bound"); compared = try wire.number("compared_paths")
        retained = try wire.number("retained_entries"); steps = try wire.number("steps")
        lastStepEntries = try wire.number("last_step_entries")
        unchanged = try wire.number("unchanged"); changed = try wire.number("changed")
        added = try wire.number("added"); removed = try wire.number("removed")
        onlyBefore = try wire.number("only_before"); onlyAfter = try wire.number("only_after")
        unavailable = try wire.number("unavailable"); knownDifferences = try wire.number("known_differences")
        uncertain = try wire.number("uncertain_paths")
        building = try wire.flag("comparison_in_progress"); scanComplete = try wire.flag("scan_complete")
        complete = try wire.flag("comparison_complete"); policiesMatch = try wire.flag("policies_match")
        membershipComparable = try wire.flag("membership_comparable")
        let identity = info.identity
        guard generation == expected, generation > 0, generation <= info.lastAttempt,
              upperBound == identity.before.members + identity.after.members, upperBound <= 131_072,
              compared <= upperBound, retained == compared, steps <= upperBound + 1, lastStepEntries <= 1024,
              [unchanged, changed, added, removed, onlyBefore, onlyAfter, unavailable].allSatisfy({ $0 <= compared }),
              unchanged + changed + added + removed + onlyBefore + onlyAfter + unavailable == compared,
              knownDifferences == changed + added + removed, uncertain == onlyBefore + onlyAfter + unavailable,
              try wire.number("max_step_entries") == 1024, scanComplete == !building,
              policiesMatch == identity.policiesMatch, membershipComparable == identity.membershipComparable,
              complete == (scanComplete && membershipComparable && uncertain == 0),
              added == 0 || (policiesMatch && identity.before.discoveryComplete),
              removed == 0 || (policiesMatch && identity.after.discoveryComplete),
              onlyBefore == 0 || !(policiesMatch && identity.after.discoveryComplete),
              onlyAfter == 0 || !(policiesMatch && identity.before.discoveryComplete),
              !scanComplete || compared >= max(identity.before.members, identity.after.members),
              building ? info.pendingGeneration == generation
                : (info.acceptedGeneration == generation && info.pendingGeneration == nil) else {
            throw AtlasSnapshotComparisonError.invalidResponse
        }
    }
    func validateSuccessor(_ next: Self, maximumEntries: UInt64) throws {
        guard building, next.generation == generation, next.upperBound == upperBound,
              next.steps == steps + 1, next.compared >= compared,
              next.compared - compared == next.lastStepEntries, next.lastStepEntries <= maximumEntries,
              next.lastStepEntries > 0 || !next.building,
              next.unchanged >= unchanged, next.changed >= changed, next.added >= added, next.removed >= removed,
              next.onlyBefore >= onlyBefore, next.onlyAfter >= onlyAfter, next.unavailable >= unavailable,
              next.policiesMatch == policiesMatch, next.membershipComparable == membershipComparable else {
            throw AtlasSnapshotComparisonError.invalidResponse
        }
    }
}

struct AtlasSnapshotComparisonPage: Identifiable, Sendable {
    let id = UUID()
    let info: AtlasSnapshotComparisonInfo
    let progress: AtlasSnapshotComparisonProgress
    let filter: AtlasSnapshotComparisonFilter
    let start, limit, total: UInt64
    let rows: [AtlasSnapshotComparisonRow]
    let next: UInt64?
    let pageLimited: Bool
    var identity: AtlasSnapshotComparisonIdentity { info.identity }
    var generation: UInt64 { progress.generation }
    var summary: String { progress.summary }
    var complete: Bool { progress.complete }
    var building: Bool { progress.building }

    static func decode(_ json: String?, identity: AtlasSnapshotComparisonIdentity, generation: UInt64,
        command: String, filter: AtlasSnapshotComparisonFilter = .differences, start: UInt64 = 0,
        limit: UInt64 = 64) throws -> Self {
        do {
            guard ["begin", "step", "page"].contains(command), (1...128).contains(limit) else {
                throw AtlasSnapshotComparisonError.invalidInput
            }
            let w = try AtlasSnapshotComparisonWire.admit(json, owner: identity.owner, command: command)
            let info = try w.info()
            guard info.identity == identity else { throw AtlasSnapshotComparisonError.invalidResponse }
            let progress = try AtlasSnapshotComparisonProgress(w, info: info, generation: generation)
            let total = try w.number("matching_entries")
            guard try w.text("filter") == filter.rawValue, try w.number("offset") == start,
                  try w.number("limit") == limit, total == progress.count(for: filter), start <= total,
                  command != "page" || !progress.building else { throw AtlasSnapshotComparisonError.invalidResponse }
            let rows = try w.array("entries").map { try AtlasSnapshotComparisonRow($0, identity: identity) }
            let maximum = min(limit, total - start), emitted = UInt64(rows.count)
            let next = try w.optionalNumber("next_offset"), limited = try w.flag("page_limited")
            guard emitted <= maximum, maximum == 0 ? emitted == 0 : emitted > 0,
                  next == (start + emitted < total ? start + emitted : nil),
                  limited == (emitted < maximum) else { throw AtlasSnapshotComparisonError.invalidResponse }
            var previous: AtlasSnapshotComparisonRow?
            for (index, row) in rows.enumerated() {
                guard row.id <= progress.retained, filter.includes(row.kind),
                      filter != .all || row.id == start + UInt64(index) + 1 else {
                    throw AtlasSnapshotComparisonError.invalidResponse
                }
                if let previous {
                    guard previous.id < row.id, previous.path.bytes.lexicographicallyPrecedes(row.path.bytes),
                          previous.before == nil || row.before == nil || previous.before!.ordinal < row.before!.ordinal,
                          previous.after == nil || row.after == nil || previous.after!.ordinal < row.after!.ordinal else {
                        throw AtlasSnapshotComparisonError.invalidResponse
                    }
                }
                previous = row
            }
            return Self(info: info, progress: progress, filter: filter, start: start, limit: limit,
                total: total, rows: rows, next: next, pageLimited: limited)
        } catch let error as AtlasSnapshotComparisonError { throw error }
        catch { throw AtlasSnapshotComparisonError.invalidResponse }
    }
}
