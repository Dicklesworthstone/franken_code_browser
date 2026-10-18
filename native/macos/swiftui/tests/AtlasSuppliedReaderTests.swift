import Foundation

/// Production model, coordinator, worker, decoders and supplied-source adapter.
/// Only foreign capture and SHA computation are injected by fixed byte vectors.
/// Native CryptoKit and pointer marshaling have a separate Mac bridge suite.
@main @MainActor private enum AtlasSuppliedReaderTests {
    typealias Fixture = AtlasSuppliedReaderFixture
    private static var checks = 0
    private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message); checks += 1
    }
    private static func wait(_ ready: () -> Bool) async throws {
        for _ in 0..<5000 {
            if ready() { return }
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure("supplied reader did not settle")
    }
    private static func source(_ text: String = "abc", path: String = "é.md") throws -> AtlasSuppliedReaderSource {
        try AtlasSuppliedReaderSource(root: "/project", path: path, text: text)
    }
    private static func opened(_ fixture: Fixture, vector: AtlasSuppliedReaderVector = .abc,
                               lease: AnyObject? = nil) async throws -> AtlasPagedReaderModel {
        let model = AtlasPagedReaderModel(transport: fixture.transport(), supplied: try source(vector.text))
        model.open(root: "/project", path: "é.md", accessLease: lease)
        try await wait { !model.busy }
        check(model.page != nil, "verified supplied source did not open")
        return model
    }

    private static func exactInputsStayOriginal() async throws {
        for vector in AtlasSuppliedReaderVector.exactCases {
            let f = Fixture(vector), model = try await opened(f, vector: vector)
            check(model.page?.originalHex == Fixture.hex(vector.text.utf8), "BOM/NUL/Unicode/newline source bytes changed")
            check(model.page?.identity.capturedBytes == UInt64(vector.text.utf8.count), "UTF-8 byte length was treated as character count")
            check(model.page?.identity.pathBytes == Array("/project/é.md".utf8), "exact label was normalized or relabeled")
            check(f.snapshot.supplied == [1] && f.snapshot.liveOpens.isEmpty, "retained opening read current disk contents")
            check(model.canRefresh && model.canPin && model.coverage.contains("already displayed"), "retained tools/provenance missing")
            check(model.findReport == nil && model.outlinePage == nil && !model.documentPreview.authorized,
                  "supplied opening inherited query/outline/document selection authority")
            model.close()
            try await wait { f.snapshot.closed[1] == 1 }
        }
    }

    private static func rejectedReceiptsNeverPublishOrFallBack() async throws {
        for fault in Fixture.Fault.allCases where fault != .none {
            let f = Fixture(); f.change { $0.fault = fault }
            let model = AtlasPagedReaderModel(transport: f.transport(), supplied: try source())
            model.open(root: "/project", path: "é.md", accessLease: nil)
            try await wait { !model.busy }
            check(model.page == nil && model.source == nil && !model.canRefresh, "bad supplied receipt was activated: \(fault)")
            check(f.snapshot.liveOpens.isEmpty, "failed import fell back to live source: \(fault)")
            if fault != .firstPageIdentity {
                check(f.snapshot.windows.isEmpty, "first page read preceded exact-source validation: \(fault)")
            }
            try await wait { f.snapshot.closed[1] == 1 }
            check(f.snapshot.retirementFailures == 0, "failed import did not retire its destination")
            model.close()
        }
    }

    private static func admissionIsBoundedAndByteSpecific() async throws {
        for path in ["", "../file", "/file", "a//b", "a/./b", "a/../b", "a\0b"] {
            do { _ = try source(path: path); preconditionFailure("invalid supplied label") }
            catch AtlasReaderError.invalidInput { checks += 1 }
        }
        let descriptor = try source()
        check(!descriptor.matches(root: "/project", path: "e\u{301}.md"), "canonical String equality was accepted as path identity")
        check(!descriptor.matches(root: "/elsewhere", path: "é.md"), "retained descriptor changed project")
        let unsupported = Fixture()
        let noCapability = AtlasPagedReaderModel(transport: unsupported.transport(supplied: false), supplied: descriptor)
        check(unsupported.snapshot.next == 1, "descriptor or model initialization began source work")
        noCapability.open(root: "/project", path: "é.md", accessLease: nil)
        check(noCapability.page == nil && !noCapability.busy && unsupported.snapshot.next == 1,
              "missing supplied capability admitted a live reader")
        let mismatch = Fixture(), wrong = AtlasPagedReaderModel(transport: mismatch.transport(), supplied: descriptor)
        wrong.open(root: "/project", path: "e\u{301}.md", accessLease: nil)
        check(wrong.page == nil && mismatch.snapshot.next == 1, "wrong raw path allocated a reader")
        let both = Fixture()
        let hit = AtlasSearchCapturedHit(root: "/project", path: "é.md", needle: "a", start: 0, end: 1,
            capturedBytes: 3, openReader: { _ in preconditionFailure("ambiguous source authority was invoked") })
        let ambiguous = AtlasPagedReaderModel(transport: both.transport(), captured: hit, supplied: descriptor)
        ambiguous.open(root: "/project", path: "é.md", accessLease: nil)
        check(ambiguous.page == nil && both.snapshot.next == 1, "two capture origins were silently prioritized")
        let tooLarge = Fixture()
        let over = try source(String(repeating: "x", count: Int(AtlasReaderLimits.captureBytes) + 1))
        let oversized = AtlasPagedReaderModel(transport: tooLarge.transport(), supplied: over)
        oversized.open(root: "/project", path: "é.md", accessLease: nil)
        try await wait { !oversized.busy }
        check(oversized.page == nil && tooLarge.snapshot.supplied.isEmpty && tooLarge.snapshot.liveOpens.isEmpty,
              "over-limit source reached foreign copying or was silently truncated")
        try await wait { tooLarge.snapshot.closed[1] == 1 }
    }

    private static func onlyExplicitRefreshObservesChangedDisk() async throws {
        let f = Fixture(), model = try await opened(f)
        let original = model.page!.identity
        model.offsetInput = "1"; model.goToByte(); try await wait { !model.busy }
        check(model.page?.originalHex == "6263" && model.canGoBack && f.snapshot.liveOpens.isEmpty,
              "navigation left retained supplied bytes after disk changed")
        f.change { $0.failLive = true }
        model.refreshFromDisk(); try await wait { !model.busy }
        check(model.page?.identity.matches(original) == true && model.page?.originalHex == "6263" && model.canGoBack,
              "failed explicit refresh discarded the supplied source/history")
        check(model.notice.hasPrefix("Refresh failed.") && !model.refreshedFromDisk, "failed refresh claimed a new capture")
        f.change { $0.failLive = false }
        model.refreshFromDisk(); try await wait { !model.busy }
        check(model.page?.text == AtlasSuppliedReaderVector.changed.text && model.refreshedFromDisk,
              "explicit refresh reused the supplied initializer")
        check(model.page?.identity.owner != original.owner && !model.canGoBack && model.canRefresh,
              "refresh retained old capture authority or history")
        let refreshed = model.page!.identity
        model.offsetInput = "8"; model.goToByte(); try await wait { !model.busy }
        check(model.page?.identity.matches(refreshed) == true && model.page?.text == "bytes\n",
              "paging after refresh returned original supplied bytes")
        check(f.snapshot.supplied == [1] && f.snapshot.liveOpens.count == 2,
              "paging performed an extra live or supplied open")
        model.close(); try await wait { f.snapshot.closed.count == 3 }
    }

    private static func pinKeepsTheSuppliedCaptureAndGrant() async throws {
        let f = Fixture()
        var lease: Fixture.Lease? = .init(f); weak var weakLease = lease
        let parent = try await opened(f, lease: lease)
        lease = nil
        let child = try await parent.fork()
        check(child.page?.originalHex == "616263" && child.page?.identity.owner != parent.page?.identity.owner,
              "pin did not retain the exact supplied bytes under an independent reader")
        parent.close(); try await wait { f.snapshot.closed[1] == 1 }
        check(weakLease != nil && f.snapshot.liveOpens.isEmpty, "pin lost its grant or reread a live file")
        child.offsetInput = "1"; child.goToByte(); try await wait { !child.busy }
        check(child.page?.originalHex == "6263", "closing source pane discarded pinned supplied bytes")
        child.refreshFromDisk(); try await wait { !child.busy }
        check(child.page?.text == AtlasSuppliedReaderVector.changed.text && f.snapshot.liveOpens.count == 1,
              "pinned supplied reader lost its explicit refresh route")
        child.close(); try await wait { weakLease == nil }
        check(f.snapshot.releasedLeases == 1 && f.snapshot.retirementFailures == 0, "grant retired more than once or leaked")
    }

    private static func lateCanceledSupplyCannotActivate() async throws {
        let f = Fixture(), gate = Fixture.Gate()
        f.change { $0.gate = gate }
        var lease: Fixture.Lease? = .init(f); weak var weakLease = lease
        let model = AtlasPagedReaderModel(transport: f.transport(), supplied: try source())
        model.open(root: "/project", path: "é.md", accessLease: lease); lease = nil
        try await wait { gate.didEnter }
        model.cancel()
        check(model.page == nil && !model.busy && weakLease != nil && f.snapshot.closed.isEmpty,
              "cancel published a source or released in-flight grant early")
        gate.release()
        try await wait { f.snapshot.closed[1] == 1 && weakLease == nil }
        check(model.page == nil && f.snapshot.windows.isEmpty && f.snapshot.liveOpens.isEmpty,
              "late supplied completion read a page or fell back after cancellation")
        // A fresh explicit retry can import the original descriptor afterward.
        model.open(root: "/project", path: "é.md", accessLease: nil)
        try await wait { !model.busy }
        check(model.page?.originalHex == "616263" && f.snapshot.supplied.count == 2, "canceled import poisoned later admission")
        model.close(); try await wait { f.snapshot.closed[2] == 1 }
    }

    static func main() async throws {
        try await exactInputsStayOriginal()
        try await rejectedReceiptsNeverPublishOrFallBack()
        try await admissionIsBoundedAndByteSpecific()
        try await onlyExplicitRefreshObservesChangedDisk()
        try await pinKeepsTheSuppliedCaptureAndGrant()
        try await lateCanceledSupplyCannotActivate()
        print("AtlasSuppliedReaderTests: \(checks) checks passed")
    }
}
