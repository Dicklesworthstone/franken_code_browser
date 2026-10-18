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
        // A post-filter over a capped all-files result loses the allowed file.
        // This independent control demonstrates that the fixture detects that
        // exact defect before exercising the production scoped request route.
        try Data(String(repeating: "scope-needle\n", count: 1001).utf8)
            .write(to: root.appendingPathComponent("a_excluded.txt"))
        try Data("scope-needle\n".utf8).write(to: root.appendingPathComponent("z_included.rs"))
        let scopedCoordinator = AtlasSearchCoordinator(scopedWork: AtlasNativeSearch.runScoped)
        var broad: AtlasSearchReport?
        try scopedCoordinator.submitScoped(input: AtlasSearchInput(root: root.path, query: "scope-needle")) {
            if let result = try? $0.get(), !result.isInProgress { broad = result }
        }
        pump { broad != nil }
        precondition(broad!.truncated && broad!.hits.count == 1000
            && broad!.hits.allSatisfy { $0.path == "a_excluded.txt" }, "Control no longer exercises capped excluded results")
        let scope = try AtlasFileScope.rust.searchScope()
        var selected: AtlasSearchReport?
        try scopedCoordinator.submitScoped(input: AtlasSearchInput(root: root.path, query: "scope-needle", fileScope: scope)) {
            do {
                let result = try $0.get()
                if !result.isInProgress { selected = result }
            } catch { preconditionFailure("Scoped search failed: \(error)") }
        }
        pump { selected != nil }
        precondition(selected!.complete && !selected!.truncated && selected!.hits.count == 1
            && selected!.hits[0].path == "z_included.rs" && selected!.matchesSeen == 1,
            "Scope must precede candidate, byte and hit limits")
        precondition(selected!.progress?.cataloguedFiles == 2 && selected!.excludedFiles == 2,
            "Scoped coverage denominator included excluded files")
        broad = nil; selected = nil; observed = nil

        try FileManager.default.createDirectory(at: root.appendingPathComponent("scoped/nested"), withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: root.appendingPathComponent("scoped2"), withIntermediateDirectories: false)
        try Data(String(repeating: "path-needle\n", count: 1001).utf8)
            .write(to: root.appendingPathComponent("a_path_noise.txt"))
        try Data(String(repeating: "path-needle\n", count: 1001).utf8)
            .write(to: root.appendingPathComponent("scoped2/noise.rs"))
        try Data(String(repeating: "file-needle\n", count: 1001).utf8)
            .write(to: root.appendingPathComponent("scoped/a_excluded.rs"))
        try Data("path-needle\nfile-needle\n".utf8).write(to: root.appendingPathComponent("scoped/z_included.rs"))
        try Data("path-needle\n".utf8).write(to: root.appendingPathComponent("scoped/nested/child.py"))
        let directory = try AtlasSearchPathScope(kind: .directory, path: "scoped")
        let file = try AtlasSearchPathScope(kind: .file, path: "scoped/z_included.rs")
        func execute(_ input: AtlasSearchInput) throws -> AtlasSearchReport {
            var answer: AtlasSearchReport?
            try scopedCoordinator.submitScoped(input: input) { result in
                do {
                    let report = try result.get()
                    if !report.isInProgress { answer = report }
                } catch { preconditionFailure("Production path-scope search failed: \(error)") }
            }
            pump { answer != nil }
            return answer!
        }
        do {
            let control = try execute(AtlasSearchInput(root: root.path, query: "path-needle"))
            precondition(control.truncated && control.hits.count == 1000
                && control.hits.allSatisfy { $0.sourcePath == "a_path_noise.txt" },
                "Directory regression must first demonstrate workspace quota starvation")
        }
        do {
            let result = try execute(AtlasSearchInput(root: root.path, query: "path-needle", pathScope: directory))
            precondition(result.complete && !result.truncated && result.matchesSeen == 2
                && Set(result.hits.compactMap(\.sourcePath)) == ["scoped/z_included.rs", "scoped/nested/child.py"],
                "Directory scope must reject sibling prefixes before consuming the match limit")
            precondition(result.progress?.cataloguedFiles == 3 && result.excludedFiles == 6,
                "Directory coverage includes descendants, not prefix siblings or outside files")
        }
        do {
            let control = try execute(AtlasSearchInput(root: root.path, query: "file-needle", pathScope: directory))
            precondition(control.truncated && control.hits.count == 1000
                && control.hits.allSatisfy { $0.sourcePath == "scoped/a_excluded.rs" },
                "Exact-file regression must demonstrate starvation by an earlier sibling in the same folder")
        }
        do {
            let result = try execute(AtlasSearchInput(root: root.path, query: "file-needle", pathScope: file))
            precondition(result.complete && !result.truncated && result.hits.count == 1
                && result.hits[0].sourcePath == "scoped/z_included.rs" && result.matchesSeen == 1,
                "Exact-file scope must precede candidate, byte and hit limits")
            precondition(result.progress?.cataloguedFiles == 1 && result.excludedFiles == 8)
        }
        do {
            let result = try execute(AtlasSearchInput(root: root.path, query: "path-needle", fileScope: scope, pathScope: directory))
            precondition(result.complete && result.hits.count == 1 && result.hits[0].sourcePath == "scoped/z_included.rs"
                && result.progress?.cataloguedFiles == 2 && result.excludedFiles == 7,
                "File-type and location predicates must intersect before traversal")
            let absent = try execute(AtlasSearchInput(root: root.path, query: "path-needle",
                pathScope: AtlasSearchPathScope(kind: .file, path: "scoped/missing.rs")))
            precondition(absent.complete && absent.hits.isEmpty && absent.excludedFiles == 9
                && absent.progress?.cataloguedFiles == 0, "Missing exact file must never widen to its directory")
        }
        print("AtlasSearchWorker: production off-main search, UTF-16, captures, file-type/file/folder limits, negatives and cancellation checks passed")
    }
}
