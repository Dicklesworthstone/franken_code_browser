import Foundation

@MainActor private final class PinnedTestReader {
    let bytes: [UInt8]
    var closes = 0
    var ready = true
    init(_ bytes: [UInt8]) { self.bytes = bytes }
}

@MainActor private final class PinFixture {
    enum Failure: Error { case refused }
    var calls = 0
    var waiting: [([UInt8], CheckedContinuation<PinnedTestReader, Error>)] = []
    var children: [PinnedTestReader] = []

    func makeStore() -> AtlasPinnedReaders<PinnedTestReader> {
        AtlasPinnedReaders(canFork: { $0.ready && $0.closes == 0 },
            fork: { [self] source in
                calls += 1
                // Deliberately ignores cancellation until its terminal reply,
                // modeling an admitted foreign call. The store must retire it.
                return try await withCheckedThrowingContinuation {
                    waiting.append((source.bytes, $0))
                }
            }, retire: { $0.closes += 1 }, failureMessage: { _ in "Pin refused" })
    }

    func complete(fail: Bool = false) {
        let (bytes, reply) = waiting.removeFirst()
        if fail { reply.resume(throwing: Failure.refused) }
        else {
            let child = PinnedTestReader(bytes)
            children.append(child)
            reply.resume(returning: child)
        }
    }
}

@main private enum AtlasPinnedReadersTests {
    @MainActor static var checks = 0
    @MainActor static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message)
        checks += 1
    }
    @MainActor static func wait(_ condition: () -> Bool) async {
        for _ in 0..<3000 {
            if condition() { return }
            await Task.yield()
        }
        preconditionFailure("Timed out awaiting pinned reader host state")
    }

    @MainActor static func main() async {
        do {
            let f = PinFixture(), store = f.makeStore()
            let first = PinnedTestReader(Array("first capture".utf8))
            let second = PinnedTestReader([0xff, 0xfe, 0x61, 0, 0, 0])
            check(!store.canAdd && f.calls == 0, "Construction is inert and has no root grant")
            store.bind(root: "/repo")
            check(store.canAdd && f.calls == 0, "Binding a root performs no source work")
            store.pin(first, root: "/other", label: "wrong")
            check(!store.busy && f.calls == 0, "A stale root cannot initiate a pin")
            first.ready = false
            store.pin(first, root: "/repo", label: "first")
            check(!store.busy && !store.notice.isEmpty, "Unavailable source is refused before fork admission")
            first.ready = true
            store.pin(first, root: "/repo", label: "first")
            await wait { f.waiting.count == 1 }
            store.pin(second, root: "/repo", label: "second")
            check(f.calls == 1 && store.entries.isEmpty, "Only one pending pin is admitted")
            f.complete()
            await wait { !store.busy }
            let firstPin = store.selected!
            check(firstPin.reader !== first && firstPin.reader.bytes == first.bytes,
                "A successful fork publishes an independently owned reader")
            check(first.closes == 0 && store.entries.count == 1, "Pinning does not close its source")
            store.bind(root: "/repo")
            check(store.selectedID == firstPin.id && firstPin.reader.closes == 0,
                "Re-enumerating the same root preserves exact pins")

            store.pin(second, root: "/repo", label: "second")
            await wait { f.waiting.count == 1 }
            f.complete()
            await wait { !store.busy }
            let secondPin = store.selected!
            check(store.entries.count == 2 && !store.canAdd && secondPin.reader.bytes == second.bytes,
                "Two pins retain independent source payloads within the host limit")
            store.pin(first, root: "/repo", label: "over limit")
            check(f.calls == 2 && !store.busy && !store.notice.isEmpty,
                "An over-limit pin does not allocate or silently replace an existing reader")
            store.select(firstPin.id)
            store.select(UUID())
            check(store.selectedID == firstPin.id && f.calls == 2,
                "Tab selection uses an admitted identity and never reopens source")
            store.close(firstPin.id)
            check(firstPin.reader.closes == 1 && secondPin.reader.closes == 0
                && store.selectedID == secondPin.id && store.canAdd,
                "Closing one reader selects its surviving neighbor without touching it")
            store.close(firstPin.id)
            check(firstPin.reader.closes == 1, "Repeated close does not retire another reader")
            store.closeAll()
            check(secondPin.reader.closes == 1 && store.entries.isEmpty && first.closes == 0 && second.closes == 0,
                "Closing the desk releases pins exactly once and leaves source readers alone")
        }
        do {
            let f = PinFixture(), store = f.makeStore(), source = PinnedTestReader([7])
            store.bind(root: "/repo")
            store.pin(source, root: "/repo", label: "kept")
            await wait { f.waiting.count == 1 }; f.complete(); await wait { !store.busy }
            let kept = store.selected!
            store.pin(source, root: "/repo", label: "refused")
            await wait { f.waiting.count == 1 }; f.complete(fail: true); await wait { !store.busy }
            check(store.selectedID == kept.id && kept.reader.closes == 0 && store.notice == "Pin refused",
                "A failed fork preserves the accepted reader and reports the failure")
            store.pin(source, root: "/repo", label: "canceled")
            await wait { f.waiting.count == 1 }
            store.cancelPin()
            check(!store.busy && store.selectedID == kept.id, "Cancel is immediate and preserves accepted pins")
            f.complete()
            await wait { f.children.last?.closes == 1 }
            check(store.entries.count == 1 && kept.reader.closes == 0,
                "A successful late foreign reply is retired, never published after cancellation")
            store.closeAll()
        }
        do {
            let f = PinFixture(), store = f.makeStore(), source = PinnedTestReader([9])
            let composed = "/r\u{e9}po", decomposed = "/re\u{301}po"
            check(composed == decomposed && !composed.utf8.elementsEqual(decomposed.utf8),
                "The root identity fixture detects Swift's canonical string equality")
            store.bind(root: composed)
            store.pin(source, root: decomposed, label: "wrong bytes")
            check(!store.busy && f.calls == 0, "Canonical Unicode equality cannot widen root authority")
            store.pin(source, root: composed, label: "old root")
            await wait { f.waiting.count == 1 }
            store.bind(root: decomposed)
            store.pin(source, root: decomposed, label: "new root")
            await wait { f.waiting.count == 2 }
            f.complete()
            await wait { f.children.first?.closes == 1 }
            check(store.busy && store.entries.isEmpty, "Old root's reply cannot clear a newer pending pin")
            f.complete(); await wait { !store.busy }
            check(store.selected?.label == "new root" && f.children.last?.closes == 0,
                "Only the new root's independently admitted reader is published")
            store.bind(root: "")
            check(store.entries.isEmpty && !store.canAdd && f.children.last?.closes == 1,
                "Revoking the root closes accepted pins and stops admission")
        }
        do {
            let f = PinFixture(), source = PinnedTestReader([1])
            var store: AtlasPinnedReaders<PinnedTestReader>? = f.makeStore()
            store!.bind(root: "/repo")
            store!.pin(source, root: "/repo", label: "closing window")
            await wait { f.waiting.count == 1 }
            weak var previous = store
            store = nil
            check(previous == nil, "Pending pin work does not retain the closed native owner")
            f.complete()
            await wait { f.children.first?.closes == 1 }
            check(source.closes == 0, "Owner teardown retires the late child without closing its source")
        }
        print("Pinned reader host lifecycle: \(checks) checks passed")
    }
}
