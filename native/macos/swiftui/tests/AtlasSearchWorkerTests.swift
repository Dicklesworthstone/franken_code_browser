import Foundation
import Dispatch

@_silgen_name("fcb_search_workspace")
private func legacySearch(_ root: UnsafePointer<CChar>?, _ query: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_free_string")
private func freeLegacySearch(_ text: UnsafeMutablePointer<CChar>?)

@_silgen_name("fcb_reader_create")
private func createRetainedReader() -> UInt64
@_silgen_name("fcb_reader_copy_range")
private func copyRetainedRange(_ handle: UInt64, _ start: UInt64, _ end: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_close")
private func closeRetainedReader(_ handle: UInt64) -> UInt8

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
        // Structural requests share the production capture owner and request
        // scheduler, while admission remains distinct from literal search.
        let structureRoot = root.appendingPathComponent("structure")
        try FileManager.default.createDirectory(at: structureRoot, withIntermediateDirectories: false)
        try Data("fn run() {}\nfn runner() {}\n// run is also plain text\n".utf8)
            .write(to: structureRoot.appendingPathComponent("source.rs"))
        let headings = Data([0xef, 0xbb, 0xbf])
            + Data("# Deploy!\r\n\r\nDeploy! is ordinary text too.\r\n\r\n# Deploy!\r\n".utf8)
        try headings.write(to: structureRoot.appendingPathComponent("README.md"))
        try Data("func run() {}\n".utf8).write(to: structureRoot.appendingPathComponent("unsupported.swift"))
        let structureScope = try AtlasSearchPathScope(kind: .directory, path: "structure")
        let rustOnly = try AtlasSearchFileScope(extensions: ["rs"])
        do {
            let result = try execute(AtlasSearchInput(root: root.path, query: "run", fileScope: rustOnly,
                pathScope: structureScope, mode: .symbols, nameMode: .exact))
            precondition(result.complete && result.hits.count == 1 && result.mode == .symbols,
                "Exact symbol name must not include its prefix neighbor or ordinary comment")
            let hit = result.hits[0]
            precondition(hit.start == 3 && hit.end == 6 && hit.structure?.name == "run"
                && hit.structure?.evidence == "heuristic-outline-candidate"
                && hit.structure?.headingSlug == nil
                && result.structureCoverage?.fileByteLimit == 65_536,
                "Symbol carries qualified metadata and original-byte name evidence")
            precondition(result.capture?.target(hit)?.mode == .symbols,
                "Captured activation must retain structural mode")
        }
        do {
            let result = try execute(AtlasSearchInput(root: root.path, query: "run", fileScope: rustOnly,
                pathScope: structureScope, mode: .symbols, nameMode: .prefix))
            precondition(result.complete && result.hits.count == 2
                && Set(result.hits.compactMap { $0.structure?.name }) == ["run", "runner"],
                "Prefix name request must reach the actual public symbol extractor")
        }
        do {
            let swiftOnly = try AtlasSearchFileScope(extensions: ["swift"])
            let result = try execute(AtlasSearchInput(root: root.path, query: "run", fileScope: swiftOnly,
                pathScope: structureScope, mode: .symbols, nameMode: .exact))
            precondition(!result.complete && result.hits.isEmpty && result.unavailableFiles == 1
                && result.unsupportedFiles == 1 && result.structureCoverage?.unsupportedFiles == 1
                && result.summary.contains("unsupported languages"),
                "Unsupported extraction is an explicit partial search, not a complete negative")
        }
        let markdownOnly = try AtlasSearchFileScope(extensions: ["md"])
        let headingInput = try AtlasSearchInput(root: root.path, query: "Deploy!", fileScope: markdownOnly,
            pathScope: structureScope, mode: .headings, nameMode: .exact)
        var retainedHeading: AtlasSearchReport?
        try scopedCoordinator.submitScoped(input: headingInput, using: { input, flag, access, publish in
            precondition(!Thread.isMainThread, "Structural capture and original-byte opening belong on the worker")
            let result = try AtlasNativeSearch.runScoped(input: input, cancellation: flag,
                access: access, progress: publish)
            precondition(result.complete && result.hits.count == 2 && result.mode == .headings
                && result.hits.map { $0.structure?.headingSlug } == ["deploy", "deploy-2"],
                "Markdown query must use canonical duplicate heading slugs")
            precondition(result.hits[0].start == 3, "Heading source range must retain the UTF-8 BOM offset")
            let hit = result.hits[1]
            guard let target = result.capture?.target(hit) else {
                preconditionFailure("Heading result lost its retained capture capability")
            }
            precondition(target.mode == .headings && target.end - target.start > UInt64(input.query.utf8.count),
                "Heading activation must retain syntax range rather than invent a literal-query range")
            try Data("# Replacement on disk\n".utf8).write(to: structureRoot.appendingPathComponent("README.md"))
            let reader = createRetainedReader()
            precondition(reader != 0)
            defer { precondition(closeRetainedReader(reader) == 1) }
            let info = try target.openReader(reader)
            precondition(info.contains("\"capture_origin\":\"host-supplied\"")
                && info.contains("\"additional_source_bytes_read\":\"0\""),
                "Opening a structural result must import the original capture without reopening its path")
            guard let copied = copyRetainedRange(reader, target.start, target.end) else {
                preconditionFailure("Retained heading bytes unavailable")
            }
            defer { freeLegacySearch(copied) }
            let length = strnlen(copied, 4 * 1024 * 1024 + 1)
            precondition(length <= 4 * 1024 * 1024)
            guard let object = try JSONSerialization.jsonObject(with: Data(bytes: copied, count: length)) as? [String: Any] else {
                preconditionFailure("Retained byte export did not return an object")
            }
            let expected = headings[Int(target.start)..<Int(target.end)]
                .map { String(format: "%02x", $0) }.joined()
            precondition(object["status"] as? String == "ok" && object["original_hex"] as? String == expected,
                "A later live edit cannot replace the bytes selected by the original heading search")
            return result
        }) { completion in
            do {
                let report = try completion.get()
                if !report.isInProgress { retainedHeading = report }
            } catch { preconditionFailure("Production structural capture flow failed: \(error)") }
        }
        pump { retainedHeading != nil }
        retainedHeading = nil
        try Data(("# " + String(repeating: "A", count: 4097) + "\n\n# A\n").utf8)
            .write(to: structureRoot.appendingPathComponent("README.md"))
        do {
            let result = try execute(AtlasSearchInput(root: root.path, query: "A", fileScope: markdownOnly,
                pathScope: structureScope, mode: .headings, nameMode: .contains))
            precondition(!result.complete && result.hits.count == 1 && result.matchesSeen == 2
                && result.unavailableFiles == 1 && result.structureCoverage?.metadataOmitted == 1
                && result.hits[0].structure?.name == "A",
                "Metadata overflow must expose omitted candidates and keep later valid heading results")
        }
        print("AtlasSearchWorker: production off-main literal/symbol/heading search, retained original-byte activation, bounded scope/coverage, negatives and cancellation checks passed")
    }
}
