import Foundation

/// These records stand in only for the already accepted native reader owner.
/// Source parsing, link resolution and capture admission remain in the ordinary
/// project/reader suites; this suite exercises the production history lifecycle.
@MainActor private final class LinkCapture: Identifiable {
    let id = UUID()
    let path: String
    let bytes: [UInt8]
    var page = 0
    var selection: Range<Int>?
    var previewOpen = false
    var closes = 0
    init(_ path: String, _ bytes: [UInt8]) { self.path = path; self.bytes = bytes }
}

@MainActor private final class LinkFixture {
    enum Failure: Error { case refused }
    var opens = 0
    var pending: [(String, [UInt8], CheckedContinuation<LinkCapture, Error>)] = []
    var returned: [LinkCapture] = []

    func makeHistory() -> AtlasDocumentLinkHistory<LinkCapture> {
        AtlasDocumentLinkHistory(retire: { $0.closes += 1 }, failureMessage: { _ in "Link refused" })
    }
    func open(_ path: String, bytes: [UInt8]) async throws -> LinkCapture {
        opens += 1
        // Ignore cancellation until the terminal reply to model an entered
        // foreign operation. The production owner must retain its slot.
        return try await withCheckedThrowingContinuation { pending.append((path, bytes, $0)) }
    }
    func finish(fail: Bool = false) {
        let (path, bytes, reply) = pending.removeFirst()
        if fail { reply.resume(throwing: Failure.refused) }
        else {
            let result = LinkCapture(path, bytes)
            returned.append(result)
            reply.resume(returning: result)
        }
    }
}

@main private enum AtlasDocumentLinkHistoryTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ result: @autoclosure () -> Bool, _ message: String) {
        precondition(result(), message)
        checks += 1
    }
    @MainActor private static func wait(_ condition: () -> Bool) async {
        for _ in 0..<3000 {
            if condition() { return }
            await Task.yield()
        }
        preconditionFailure("Timed out awaiting document link history")
    }
    @MainActor private static func accept(_ history: AtlasDocumentLinkHistory<LinkCapture>,
        _ fixture: LinkFixture, path: String, bytes: [UInt8]) async -> LinkCapture {
        history.follow { try await fixture.open(path, bytes: bytes) }
        await wait { fixture.pending.count == 1 }
        fixture.finish()
        await wait { !history.busy }
        return fixture.returned.last!
    }

    @MainActor static func main() async {
        do {
            let fixture = LinkFixture(), history = fixture.makeHistory()
            let origin = LinkCapture("src/lib.rs", [0x66, 0x6e])
            check(!history.canFollow && history.current == nil && fixture.opens == 0,
                "Construction is inert and confers no project authority")
            history.follow { try await fixture.open("unbound", bytes: []) }
            check(fixture.opens == 0 && !history.busy, "An unbound history cannot open source")
            check(!history.bind(root: "/repo", accessIdentity: nil), "First binding establishes an explicit root")
            check(history.canFollow && fixture.opens == 0, "Binding performs no file work")

            let docs = await accept(history, fixture, path: "docs/design.md", bytes: Array("# exact docs".utf8))
            docs.page = 7
            docs.selection = 2..<5
            docs.previewOpen = true
            let declaration = await accept(history, fixture, path: "src/lib.rs", bytes: [0xff, 0xfe, 0x66, 0])
            declaration.page = 11
            let opens = fixture.opens
            history.back()
            check(history.current === docs && docs.page == 7 && docs.selection == 2..<5 && docs.previewOpen,
                "source-to-docs-to-declaration-to-back restores the same reader and its place")
            history.forward()
            check(history.current === declaration && declaration.page == 11
                && declaration.bytes == [0xff, 0xfe, 0x66, 0],
                "Forward preserves exact original bytes and the independent capture identity")
            check(fixture.opens == opens && origin.closes == 0 && docs.closes == 0,
                "History navigation has no source operation and never closes the originating source")
            check(history.bind(root: "/repo", accessIdentity: nil) && history.current === declaration,
                "A same-root refresh preserves retained captures")
            history.catalogRefreshed()
            history.back()
            check(history.current === docs && fixture.opens == opens,
                "Retained history remains usable after catalog replacement")

            history.follow { try await fixture.open("missing.md", bytes: []) }
            await wait { fixture.pending.count == 1 }
            history.back()
            check(history.current === docs && !history.canForward, "Pending navigation does not activate another source")
            fixture.finish(fail: true)
            await wait { !history.busy }
            check(history.current === docs && history.canForward && declaration.closes == 0
                && history.notice == "Link refused",
                "A failed link keeps the accepted source and its Forward history")

            let branch = await accept(history, fixture, path: "docs/other.md", bytes: [7])
            check(history.current === branch && !history.canForward && declaration.closes == 1
                && docs.closes == 0 && history.captures.count == 2,
                "Only successful navigation replaces the abandoned Forward branch")
            history.close()
            history.close()
            check(docs.closes == 1 && branch.closes == 1 && origin.closes == 0,
                "Closing a link trail retires each owned reader once and preserves the origin")
        }
        do {
            let fixture = LinkFixture(), history = fixture.makeHistory()
            history.bind(root: "/repo", accessIdentity: nil)
            let first = await accept(history, fixture, path: "same.md", bytes: [1])
            let second = await accept(history, fixture, path: "second.md", bytes: [2])
            let third = await accept(history, fixture, path: "third.md", bytes: [3])
            history.follow { try await fixture.open("same.md", bytes: [4]) }
            await wait { fixture.pending.count == 1 }
            check(history.captures.count == 3 && first.closes == 0 && third.closes == 0,
                "A full trail admits only its one candidate and does not evict before acceptance")
            history.follow { try await fixture.open("extra.md", bytes: [9]) }
            check(fixture.opens == 4 && fixture.pending.count == 1, "There is no queued second candidate")
            fixture.finish()
            await wait { !history.busy }
            let fourth = fixture.returned.last!
            check(history.captures.count == 3 && history.current === fourth && first.closes == 1,
                "The oldest linked source expires only after the next capture is accepted")
            check(fourth.path == first.path && fourth.bytes != first.bytes && fourth !== first,
                "An equal pathname never substitutes for the retained source identity")
            history.back()
            check(history.current === third && third.closes == 0, "Back uses the nearest retained source")
            history.back()
            history.back()
            check(history.current === second && !history.canBack && fixture.opens == 4,
                "The history bound is explicit; expired sources are not silently reopened")
            history.close()
            check(second.closes == 1 && third.closes == 1 && fourth.closes == 1,
                "Every surviving bounded capture retires on close")
        }
        do {
            let fixture = LinkFixture(), history = fixture.makeHistory()
            history.bind(root: "/repo", accessIdentity: nil)
            let kept = await accept(history, fixture, path: "kept.md", bytes: [5])
            history.follow { try await fixture.open("late.md", bytes: [6]) }
            await wait { fixture.pending.count == 1 }
            history.cancel()
            check(history.busy && history.canceling && !history.canFollow && history.current === kept,
                "Cancellation revokes publication but retains admission until drain")
            history.follow { try await fixture.open("overlap.md", bytes: []) }
            check(fixture.opens == 2 && fixture.pending.count == 1,
                "Rapid retry cannot overlap a canceled foreign call")
            fixture.finish()
            await wait { !history.busy }
            check(fixture.returned.last?.closes == 1 && history.current === kept
                && history.captures.count == 1 && history.canFollow,
                "A late successful reply retires without replacing the accepted source")
            history.follow { try await fixture.open("old-catalog.md", bytes: [8]) }
            await wait { fixture.pending.count == 1 }
            history.catalogRefreshed()
            check(history.busy && history.current === kept, "Refresh cancels pending catalog work while retaining source")
            fixture.finish()
            await wait { !history.busy }
            check(history.current === kept && kept.closes == 0 && fixture.returned.last?.closes == 1,
                "A refreshed catalog cannot publish an old link proposal")
            history.close()
        }
        do {
            let fixture = LinkFixture(), history = fixture.makeHistory()
            let grant = NSObject(), replacementGrant = NSObject()
            let composed = "/r\u{e9}po", decomposed = "/re\u{301}po"
            check(composed == decomposed && !composed.utf8.elementsEqual(decomposed.utf8),
                "The fixture distinguishes Swift equality from native path bytes")
            history.bind(root: composed, accessIdentity: ObjectIdentifier(grant))
            let kept = await accept(history, fixture, path: "kept.md", bytes: [9])
            history.follow { try await fixture.open("old-root.md", bytes: [10]) }
            await wait { fixture.pending.count == 1 }
            check(!history.bind(root: decomposed, accessIdentity: ObjectIdentifier(grant)),
                "A canonically equivalent but byte-distinct root revokes history")
            check(kept.closes == 1 && history.current == nil && history.busy && !history.canFollow,
                "Root replacement closes accepted readers but retains the draining slot")
            fixture.finish()
            await wait { !history.busy }
            check(fixture.returned.last?.closes == 1 && history.current == nil && history.canFollow,
                "An old-root reply cannot populate the new root's history")
            let replacement = await accept(history, fixture, path: "new.md", bytes: [11])
            check(!history.bind(root: decomposed, accessIdentity: ObjectIdentifier(replacementGrant)),
                "Replacing native access revokes history even for identical path bytes")
            check(replacement.closes == 1 && history.current == nil,
                "Same-path grant replacement retires the prior captures")
            history.bind(root: "", accessIdentity: nil)
            check(!history.bound && !history.canFollow, "Revocation prevents further local-source admission")
        }
        do {
            let fixture = LinkFixture(), history = fixture.makeHistory()
            history.bind(root: "/repo", accessIdentity: nil)
            let kept = await accept(history, fixture, path: "kept.md", bytes: [12])
            history.follow { try await fixture.open("closing.md", bytes: [13]) }
            await wait { fixture.pending.count == 1 }
            history.close()
            check(history.current == nil && history.busy && kept.closes == 1,
                "Closing withdraws the trail immediately while foreign work drains")
            fixture.finish()
            await wait { !history.busy }
            check(fixture.returned.last?.closes == 1 && !history.bound && history.notice.isEmpty,
                "A late reply cannot reopen a dismissed sheet or grant")
        }
        do {
            let fixture = LinkFixture()
            var history: AtlasDocumentLinkHistory<LinkCapture>? = fixture.makeHistory()
            history!.bind(root: "/repo", accessIdentity: nil)
            history!.follow { try await fixture.open("orphan.md", bytes: [14]) }
            await wait { fixture.pending.count == 1 }
            weak var previous = history
            history = nil
            check(previous == nil, "Pending link work does not retain its dismissed native owner")
            fixture.finish()
            await wait { fixture.returned.last?.closes == 1 }
            check(fixture.returned.last?.closes == 1, "An orphaned late capture is retired")
        }
        print("Document link history: \(checks) checks passed")
    }
}
