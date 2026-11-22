import Foundation
import Dispatch

/// Fixed FCBT producer receipts exercise production admission, recovery and
/// independent readers. Rust owns format validation and readiness resolution.
final class SavedTrailFixture: @unchecked Sendable {
    struct State {
        var generation: UInt64?
        var attempt: UInt64 = 0
        var path = "/saved/guide.fcbt"
        var total: UInt64 = 34
    }
    let base = SavedRepositoryFixture(searchVectors: true)
    let resume = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var states: [UInt64: State] = [:]
    private var fault: String?
    private var loseInfo = false
    private var block = false
    private var entered = false
    private var log: [String] = []
    let digest = String(repeating: "a", count: 64)
    private func locked<T>(_ work: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return work() }
    var calls: [String] { locked { log } }
    var didEnter: Bool { locked { entered } }
    func corrupt(_ kind: String) { locked { fault = kind } }
    func blockAttach() { locked { block = true; entered = false } }
    func object(_ json: String?) -> [String: Any] {
        try! JSONSerialization.jsonObject(with: Data(json!.utf8)) as! [String: Any]
    }
    func entry(_ item: UInt64) -> [String: Any] {
        let ordinal: UInt64 = item == 6 || item == 7 || item == 32 ? 1 : (item == 2 ? 4 : 0)
        let member = base.members[Int(ordinal)], bytes = UInt64(member.bytes?.count ?? 99)
        let readiness = [UInt64(1): "other-archive", 2: "unavailable-member", 3: "changed-source",
                         4: "missing-member", 5: "invalid-range"][item] ?? "ready"
        let ready = readiness == "ready"
        let start: UInt64 = item == 7 ? bytes : (item == 32 ? 202 : 0)
        let end = item == 33 ? 0 : bytes
        return ["item": String(item), "archive_digest": item == 1 ? String(repeating: "e", count: 64) : base.archiveDigest,
            "source_digest": item == 3 ? String(repeating: "b", count: 64) : member.digest ?? digest,
            "source_bytes": String(bytes), "path": item == 4 ? base.path(Array("absent.rs".utf8), "absent.rs")
                : base.path(member.path, member.display),
            "original_range": ["start": String(start), "end": String(end)],
            "rationale": item == 1 ? "Compare the other saved revision." : "Visit \(item + 1): e\u{301} — keep this exact note.",
            "evidence": "explicit-user-selection", "readiness": readiness,
            "member": ready ? String(ordinal) as Any : NSNull(),
            "file_id": ready ? String(ordinal + 1) as Any : NSNull(),
            "source_revision": ready ? String(ordinal + 1) as Any : NSNull()]
    }
    func page(_ owner: UInt64, generation: UInt64 = 1, attempt: UInt64 = 1, path: String = "/saved/guide.fcbt",
        total: UInt64 = 34, start: UInt64 = 0, limit: UInt64 = 32, command: String = "trail-page") -> [String: Any] {
        let rows = (start..<min(total, start + limit)).map(entry)
        let end = start + UInt64(rows.count), unavailable = min(total > 0 ? total - 1 : 0, 5)
        let referenceBytes = (0..<total).reduce(UInt64(0)) { sum, item in
            let range = entry(item)["original_range"] as! [String: String]
            return sum + UInt64(range["end"]!)! - UInt64(range["start"]!)!
        }
        var out = base.header(owner, command)
        out.merge(["trail_generation": String(generation), "accepted_trail_generation": String(generation),
            "last_trail_attempt": String(attempt), "trail_digest": digest, "trail_path": base.path(Array(path.utf8), path),
            "title": "A saved reading journey", "parent_digest": NSNull(), "total_items": String(total),
            "reference_bytes": String(referenceBytes), "retained_trail_bytes": "8192", "ready_items": String(total - unavailable),
            "unavailable_items": String(unavailable), "trail_read_calls": "2", "max_trail_bytes": "4194304",
            "max_trail_items": "1024", "max_page_items": "32", "readiness_scope": "selected-saved-archive-metadata",
            "payload_verified": false, "source_observation": "saved-manifest-metadata", "truncated": false,
            "retained_source_payload_bytes": "0", "member_bytes_read": "0", "member_read_calls": "0",
            "offset": String(start), "limit": String(limit), "items": rows,
            "next_offset": end < total ? String(end) as Any : NSNull()]) { _, new in new }
        return out
    }
    var saved: AtlasSavedTransport {
        let inner = base.savedTransport
        return AtlasSavedTransport(create: inner.create, run: { [self] owner, request in
            let response = inner.run(owner, request)
            switch request {
            case .open, .info:
                return locked {
                    if case .open = request { states[owner] = State() }
                    if case .info = request, loseInfo { loseInfo = false; return nil }
                    guard let response else { return nil }
                    let state = states[owner] ?? State()
                    var out = object(response)
                    out["accepted_trail_generation"] = state.generation.map { String($0) as Any } ?? NSNull()
                    out["last_trail_attempt"] = String(state.attempt)
                    return base.encode(out)
                }
            default: return response
            }
        }, cancel: inner.cancel, close: inner.close, retirementFailed: inner.retirementFailed, readerDomain: base.domain)
    }
    var trails: AtlasSavedTrailTransport {
        AtlasSavedTrailTransport(run: { [self] owner, request in
            precondition(!Thread.isMainThread)
            if case .attach = request {
                let pause = locked { let value = block; block = false; entered = value; return value }
                if pause { precondition(resume.wait(timeout: .now() + .seconds(5)) == .success) }
            }
            return locked {
                var state = states[owner] ?? State()
                switch request {
                case .attach(let generation, let path):
                    log.append("attach:\(generation):\(path)")
                    precondition(generation > state.attempt); state.attempt = generation
                    if fault == "attach-failed" {
                        fault = nil; states[owner] = state
                        return base.encode(["schema": "fcb.saved-repository/1", "status": "error",
                            "owner": String(owner), "error": ["code": "TRAIL_CHECKSUM"]])
                    }
                    state.generation = generation; state.path = path; state.total = path.hasSuffix("empty.fcbt") ? 0 : 34
                    states[owner] = state
                    if fault == "attach-lost" || fault == "attach-and-info-lost" {
                        loseInfo = fault == "attach-and-info-lost"; fault = nil; return nil
                    }
                    return base.encode(page(owner, generation: generation, attempt: generation,
                        path: path, total: state.total, command: "trail-attach"))
                case .page(let generation, let start, let limit):
                    log.append("page:\(generation):\(start)"); precondition(state.generation == generation)
                    return base.encode(page(owner, generation: generation, attempt: state.attempt,
                        path: state.path, total: state.total, start: start, limit: limit))
                case .detach(let generation):
                    log.append("detach:\(generation)"); precondition(generation > state.attempt)
                    state.attempt = generation; state.generation = nil; states[owner] = state
                    if fault == "detach-lost" { fault = nil; return nil }
                    var out = base.header(owner, "trail-detach")
                    out["accepted_trail_generation"] = NSNull(); out["last_trail_attempt"] = String(generation)
                    return base.encode(out)
                case .openReader(let reader, let generation, let item):
                    log.append("reader:\(generation):\(item)"); precondition(state.generation == generation)
                    let row = entry(item), member = UInt64(row["member"] as! String)!
                    if fault == "reader-lost" { base.corruptNext("lost"); fault = nil }
                    guard let response = base.savedTransport.run(owner, .openMember(reader: reader, member: member)) else { return nil }
                    var out = object(response)
                    let range = row["original_range"] as! [String: String], start = UInt64(range["start"]!)!
                    out["command"] = "trail-open-reader"; out["trail_generation"] = String(generation)
                    out["trail_digest"] = digest; out["item"] = String(item); out["entry"] = row
                    out["source_bytes"] = row["source_bytes"]; out["path"] = row["path"]
                    out["offset"] = String(start); out["selection"] = range
                    out["window"] = object(base.readerTransport().window(reader, start > 132 ? start - 132 : 0, 16 * 1024))
                    if let error = fault {
                        fault = nil
                        switch error {
                        case "reader-range": out["selection"] = ["start": "0", "end": "1"]
                        case "reader-generation": out["trail_generation"] = String(generation + 1)
                        case "reader-digest": out["trail_digest"] = String(repeating: "f", count: 64)
                        case "reader-entry": var wrong = row; wrong["rationale"] = "changed note"; out["entry"] = wrong
                        default: preconditionFailure("unknown trail fixture fault")
                        }
                    }
                    return base.encode(out)
                }
            }
        })
    }
}

@main private enum AtlasSavedTrailTests {
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) async {
        for _ in 0..<2000 { if ready() { return }; try? await Task.sleep(nanoseconds: 1_000_000) }
        preconditionFailure(message)
    }
    private static func reject(_ work: () throws -> Void) {
        do { try work(); preconditionFailure("invalid trail receipt accepted") } catch {}
    }
    @MainActor static func main() async throws {
        let fixture = SavedTrailFixture()
        let model = AtlasSavedRepositoryModel(transport: fixture.saved, trails: fixture.trails)
        model.open(path: "/saved/repository.fcbs", accessLease: nil)
        await wait("archive open") { !model.busy }
        let identity = model.info!.identity
        let decode: ([String: Any]) throws -> AtlasSavedTrailPage = {
            try AtlasSavedTrailPage.decode(fixture.base.encode($0), identity: identity, generation: 1, start: 0)
        }
        let page = try decode(fixture.page(identity.owner))
        precondition(page.items.count == 32 && page.next == 32 && page.ready == 29 && page.unavailable == 5)
        precondition(page.items[1].readiness == .otherArchive && !page.items[1].canOpen)
        precondition(page.items[6].path.bytes == fixture.base.rawPath)
        precondition(page.items[7].range.start == page.items[7].range.end, "point bookmarks must survive admission")
        for (field, value) in [("owner", "0" as Any), ("trail_generation", "2"), ("last_trail_attempt", "0"),
            ("payload_verified", true), ("source_observation", "live-file"), ("retained_source_payload_bytes", "1"),
            ("member_read_calls", "1"), ("reference_bytes", "0"), ("total_items", "1025"), ("ready_items", "34"), ("limit", "33"),
            ("max_page_items", "64"), ("next_offset", "31"), ("truncated", true), ("parent_digest", "invalid")] {
            var invalid = fixture.page(identity.owner); invalid[field] = value; reject { _ = try decode(invalid) }
        }
        for (field, value) in [("item", "1" as Any), ("file_id", "9"), ("archive_digest", String(repeating: "e", count: 64)),
            ("source_revision", NSNull()), ("rationale", String(repeating: "x", count: 4097)),
            ("original_range", ["start": "9", "end": "8"]), ("evidence", "inferred-relevance")] {
            var invalid = fixture.page(identity.owner), rows = invalid["items"] as! [[String: Any]]
            rows[0][field] = value; invalid["items"] = rows; reject { _ = try decode(invalid) }
        }
        var reordering = fixture.page(identity.owner), rows = reordering["items"] as! [[String: Any]]
        rows.swapAt(0, 1); reordering["items"] = rows; reject { _ = try decode(reordering) }
        var unavailableID = fixture.page(identity.owner); rows = unavailableID["items"] as! [[String: Any]]
        rows[1]["member"] = "0"; unavailableID["items"] = rows; reject { _ = try decode(unavailableID) }

        model.attachTrail(path: "/saved/guide.fcbt", accessLease: nil)
        await wait("trail attach") { !model.busy }
        precondition(model.trailIsCurrent && model.trailGeneration == 1 && model.selectedTrailItem == 0)
        let capture = try await model.openTrailItem(model.trailPage!.items[0], from: model.trailPage!,
            readerTransport: fixture.base.readerTransport())
        precondition(capture.match == nil && capture.canUseTrailRange && !capture.reader.canRefresh)
        let beforeUnavailable = fixture.base.calls.filter { $0.hasPrefix("reader-create") }.count
        do {
            _ = try await model.openTrailItem(at: 1, from: capture, readerTransport: fixture.base.readerTransport())
            preconditionFailure("other-archive entry opened")
        } catch AtlasSavedError.unavailableTrailItem(_) { }
        precondition(model.selectedTrailEntry?.id == 1 && model.selectedTrailEntry?.rationale.contains("other saved") == true)
        precondition(fixture.base.calls.filter { $0.hasPrefix("reader-create") }.count == beforeUnavailable)
        let later = try await model.openTrailItem(at: 32, from: capture, readerTransport: fixture.base.readerTransport())
        precondition(model.trailPage?.start == 32 && later.reader.page?.start == 70 && later.canUseTrailRange)
        precondition(later.member.path.bytes == fixture.base.rawPath)
        let point = try await model.openTrailItem(at: 33, from: later, readerTransport: fixture.base.readerTransport())
        precondition(point.canUseTrailRange && point.evidenceLabel.contains("Bookmark at byte 0"))

        for fault in ["reader-range", "reader-generation", "reader-digest", "reader-entry", "reader-lost"] {
            fixture.corrupt(fault)
            do {
                _ = try await model.openTrailItem(at: 32, from: point, readerTransport: fixture.base.readerTransport())
                preconditionFailure("corrupt trail handoff accepted")
            } catch { }
            precondition(!model.busy && point.canUseTrailRange)
        }
        fixture.corrupt("attach-failed")
        model.attachTrail(path: "/saved/bad.fcbt", accessLease: nil)
        await wait("failed attach recovery") { !model.busy }
        precondition(model.trailGeneration == 1 && model.trailIsCurrent && model.trailPage?.start == 32)
        fixture.corrupt("attach-lost")
        model.attachTrail(path: "/saved/replacement.fcbt", accessLease: nil)
        await wait("lost attach recovery") { !model.busy }
        precondition(model.trailGeneration == 3 && model.trailIsCurrent && model.trailPage?.start == 0)
        precondition(!model.canNavigateTrail(from: capture) && capture.canUseTrailRange)
        fixture.corrupt("attach-and-info-lost")
        model.attachTrail(path: "/saved/unknown.fcbt", accessLease: nil)
        await wait("uncertain attach") { !model.busy }
        precondition(!model.trailStateKnown && !model.trailIsCurrent && !model.canOpenTrail && model.trailPage?.generation == 3)
        model.reloadTrail(); await wait("reload accepted trail") { !model.busy }
        precondition(model.trailGeneration == 4 && model.trailIsCurrent)
        precondition(fixture.calls.filter { $0 == "attach:4:/saved/unknown.fcbt" }.count == 1, "lost attachment must never replay pathname open")
        fixture.corrupt("detach-lost"); model.detachTrail()
        await wait("lost detach recovery") { !model.busy }
        precondition(model.trailPage == nil && model.trailStateKnown)
        model.attachTrail(path: "/saved/empty.fcbt", accessLease: nil)
        await wait("empty trail") { !model.busy }
        precondition(model.trailPage?.total == 0 && model.trailPage?.items.isEmpty == true && model.trailPage?.next == nil)

        final class Grant { }
        var grant: Grant? = Grant()
        weak var weakGrant = grant
        fixture.blockAttach(); model.attachTrail(path: "/saved/canceled.fcbt", accessLease: grant); grant = nil
        await wait("blocked attachment entered") { fixture.didEnter }
        model.cancel(); precondition(model.busy && weakGrant != nil, "selected-file grant retired while entered work used it")
        fixture.resume.signal(); await wait("canceled attachment drained") { !model.busy }
        await wait("grant released after drain") { weakGrant == nil }
        precondition(model.trailStateKnown && model.trailIsCurrent)
        model.closeArchive()
        precondition(model.trailPage == nil && !model.canNavigateTrail(from: point) && point.canUseTrailRange)
        precondition(!fixture.base.calls.contains("LIVE-OPEN"), "trail navigation reopened live source")
        capture.reader.close(); later.reader.close(); point.reader.close(); model.close()
        try await AtlasSavedTrailArchiveReaderTests.run()
        try await AtlasSavedTrailAuthoringTests.run()
        try await AtlasSavedTrailExportTests.run()
        print("Saved trail wire, ordered navigation, exact ranges, uncertain replies and retained-reader fixtures passed")
    }
}
