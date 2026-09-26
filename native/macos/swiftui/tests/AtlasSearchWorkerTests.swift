import Foundation
import Dispatch

@_silgen_name("fcb_search_workspace")
private func legacySearch(_ root: UnsafePointer<CChar>?, _ query: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_free_string")
private func freeLegacySearch(_ text: UnsafeMutablePointer<CChar>?)

/// Link AtlasSearch.swift, AtlasSearchCoordinator.swift, AtlasSearchWorker.swift
/// and the real fcb-bridge static library. These tests use actual temporary
/// sources and the production worker, not fabricated search responses. AppKit,
/// security-scoped bookmarks and physical-Mac frame latency remain separate.
@MainActor @main struct AtlasSearchWorkerTests {
    static func pump(until condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(10)
        while !condition() && Date() < deadline {
            _ = RunLoop.main.run(mode: .default, before: Date().addingTimeInterval(0.005))
        }
        precondition(condition(), "native bridge worker timed out")
    }
    static func main() throws {
        precondition(Thread.isMainThread)
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("fcb-native-search-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        try Data("needle first\nneedle second\n".utf8).write(to: root.appendingPathComponent("source.rs"))
        let unicode = "needle😀\r\n"
        var utf16 = Data([0xff, 0xfe])
        for unit in unicode.utf16 { utf16.append(UInt8(unit & 255)); utf16.append(UInt8(unit >> 8)) }
        try utf16.write(to: root.appendingPathComponent("utf16.txt"))

        let coordinator = AtlasSearchCoordinator { root, query, flag in
            precondition(!Thread.isMainThread, "production bridge ran on the UI thread")
            let actual = try AtlasNativeSearch.run(root: root, query: query, cancellation: flag)
            let raw = root.withCString { root in query.withCString { legacySearch(root, $0) } }
            defer { freeLegacySearch(raw) }
            guard let raw else { preconditionFailure("legacy control failed") }
            let control = try AtlasSearchReport.decode(String(cString: raw))
            precondition(actual.hits == control.hits && actual.complete == control.complete
                && actual.truncated == control.truncated && actual.matchesSeen == control.matchesSeen,
                "cancelable route changed source or coverage semantics")
            return actual
        }
        var observed: AtlasSearchReport?
        try coordinator.submit(root: root.path, query: "needle") { result in
            precondition(Thread.isMainThread)
            do { observed = try result.get() } catch { preconditionFailure("search failed: \(error)") }
        }
        pump { observed != nil }
        precondition(observed!.hits.count == 3)
        let encodedHit = observed!.hits.first { $0.path == "utf16.txt" }!
        precondition(encodedHit.start == 2 && encodedHit.end == 14, "UTF-16 original-byte range changed")
        precondition(observed!.hits.allSatisfy { $0.captureSHA256 != nil && $0.captureByteLength != nil })

        observed = nil
        try coordinator.submit(root: root.path, query: "absent") { result in
            do { observed = try result.get() } catch { preconditionFailure("negative query failed") }
        }
        pump { observed != nil }
        precondition(observed!.complete && observed!.hits.isEmpty, "complete negative confused with failure")

        let canceled = AtlasSearchCancellation(); canceled.cancel()
        do {
            _ = try AtlasNativeSearch.run(root: root.appendingPathComponent("missing").path,
                                          query: "needle", cancellation: canceled)
            preconditionFailure("pre-canceled native work executed")
        } catch AtlasSearchError.canceled {}

        for invalid in ["", "needle\0ignored", String(repeating: "a", count: 1025)] {
            do {
                _ = try AtlasNativeSearch.run(root: root.path, query: invalid, cancellation: AtlasSearchCancellation())
                preconditionFailure("invalid C-string input accepted")
            } catch AtlasSearchError.invalidRequest {}
        }
        print("AtlasSearchWorker: production off-main search, UTF-16, exact captures, negatives and cancellation checks passed")
    }
}
