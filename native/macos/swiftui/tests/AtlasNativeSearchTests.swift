// Execute the production worker/codec against fixed C symbols. No Rust or OS I/O.
import Foundation

@_silgen_name("fixture_reset") private func resetFixture()
@_silgen_name("fixture_set") private func setFixture(_ kind: UInt32, _ index: UInt32, _ text: UnsafePointer<CChar>)
@_silgen_name("fixture_count") private func fixtureCount(_ key: UInt32) -> UInt32
@_silgen_name("fixture_cancel_step") private func cancelStep()
@_silgen_name("fixture_refuse_create") private func refuseCreate()
@_silgen_name("fixture_expect_scope") private func expectScope(_ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?)
@_silgen_name("fixture_expect_path") private func expectPath(_ kind: UInt64, _ path: UnsafePointer<CChar>)
@_silgen_name("fixture_expect_counted_case") private func expectCountedCase(_ caseSensitive: UInt64)
@_silgen_name("fixture_expect_policy") private func expectPolicy(_ policy: UInt64)
@_silgen_name("fixture_expect_directory") private func expectDirectory(_ bytes: UnsafePointer<UInt8>?, _ length: UInt64)
@_silgen_name("fixture_expect_file") private func expectFile(_ bytes: UnsafePointer<UInt8>?, _ length: UInt64)
@_silgen_name("fixture_expect_structure") private func expectStructure(_ mode: UInt64, _ nameMode: UInt64)
@_cdecl("fixture_mark_canceled") public func markCanceled(_ context: UnsafeMutableRawPointer?) {
    guard let context else { preconditionFailure("Missing callback lifetime") }
    Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().cancel()
}
private final class Reports: @unchecked Sendable {
    private let lock = NSLock()
    private var storage: [AtlasSearchReport] = []
    func append(_ report: AtlasSearchReport) { lock.lock(); storage.append(report); lock.unlock() }
    func clear() { lock.lock(); storage = []; lock.unlock() }
    var values: [AtlasSearchReport] { lock.lock(); defer { lock.unlock() }; return storage }
}

@main private enum AtlasNativeSearchTests {
    private static func hit(_ id: Int, path: String = "612e7273") -> [String: Any] {
        ["hit_id": String(id), "file_id": "1", "source_revision": "1",
         "capture_sha256": String(repeating: "a", count: 64), "capture_byte_length": "1000",
         "original_range": ["start": String(id - 1), "end": String(id)],
         "path": ["encoding": "unix-bytes", "hex": path, "display": "a.rs"]]
    }
    private static func page(_ command: String, step: Int, total: Int = 0, start: Int = 0,
                             running: Bool = true, complete: Bool = false, catalogued: Int = 2) -> [String: Any] {
        let end = min(total, start + (command == "page" ? 128 : 64))
        let rows: [[String: Any]] = start < end ? ((start + 1)...end).map { hit($0) } : []
        return ["schema": "fcb.atlas-search/1", "status": "ok", "command": command,
            "owner": "11", "source_manifest": "1", "layout_revision": "1", "query_generation": "1",
            "needle": "x", "mode": "exact-decoded-literal", "search_strategy": "live-capture-scan",
            "discovery_policy": "repository-rules-v1",
            "discovery_complete": true, "search_in_progress": running, "search_complete": complete,
            "count_all_matches": true, "match_count_complete": complete,
            "truncated": false, "stop_reason": running ? NSNull() : "all-files-examined",
            "catalogued_files": String(catalogued), "examined_files": String(min(step, catalogued)),
            "scanned_files": String(min(step, catalogued)), "unavailable_files": "0",
            "pending_files": String(max(0, catalogued - step)), "matches_seen": String(total),
            "retained_hits": String(total), "displayed_hits": String(total), "step_count": String(step),
            "hits": rows, "next_offset": end < total ? String(end) : NSNull()]
    }
    private static func countedPage(_ command: String, step: Int, start: Int = 0,
                                    caseSensitive: Bool) -> [String: Any] {
        var value = page(command, step: step, total: step == 0 ? 0 : 1000,
            start: start, running: step < 2, complete: step == 2)
        value["case_sensitive"] = caseSensitive
        value["mode"] = AtlasSearchText.wireMode(caseSensitive: caseSensitive)
        value["matches_seen"] = step == 2 ? "1200" : (step == 0 ? "0" : "1000")
        value["truncated"] = step == 2
        if !caseSensitive, let rows = value["hits"] as? [[String: Any]] {
            value["hits"] = rows.map { row in var row = row; row["matched_text"] = "X"; return row }
        }
        return value
    }
    private static func importReply() -> [String: Any] {
        var reply = hit(1)
        reply.merge(["schema": "fcb.atlas-search/1", "status": "ok", "command": "open-reader",
            "owner": "11", "source_manifest": "1", "layout_revision": "1", "query_generation": "1",
            "reader_owner": "100", "source_reopened": false, "source_observation": "retained-search-capture",
            "reader": ["schema": "fcb.reader-session/1", "status": "ok", "command": "info",
                "owner": "100", "file_id": "1", "source_revision": "1", "captured_bytes": "1000",
                "path": ["encoding": "unix-bytes", "hex": "612e7273", "display": "a.rs"],
                "capture_origin": "host-supplied", "initial_source_bytes_read": "0", "initial_read_calls": "0",
                "additional_source_bytes_read": "0", "native_presented": false, "encoding": "utf16le"]], uniquingKeysWith: { _, new in new })
        return reply
    }
    private static func install(_ kind: UInt32, _ object: [String: Any], index: UInt32 = 0) throws {
        let data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        String(decoding: data, as: UTF8.self).withCString { setFixture(kind, index, $0) }
    }
    private static func openReceipt(_ policy: String = "repository-rules-v1") -> [String: Any] {
        ["schema": "fcb.atlas-session/1", "status": "ok", "command": "info",
         "owner": "11", "discovery_policy": policy]
    }
    private static func setup() throws {
        resetFixture()
        try install(0, openReceipt())
        try install(1, page("begin", step: 0))
        try install(2, page("step", step: 1, total: 70))
        try install(3, page("page", step: 1, total: 70, start: 64))
        try install(2, page("step", step: 2, total: 70, running: false, complete: true), index: 1)
        try install(4, importReply())
    }
    private static func scopedPage(_ command: String, scope: AtlasSearchFileScope, step: Int,
                                   eligible: Int, total: Int = 0, running: Bool = true,
                                   pathScope: AtlasSearchPathScope = .workspace, hitPath: String? = nil) -> [String: Any] {
        var value = page(command, step: step, total: total, running: running, complete: !running)
        value["search_scope"] = scope.wireKind
        value["scope_extensions"] = scope.extensions ?? ""
        value["scope_filenames"] = scope.filenames ?? ""
        value["eligible_files"] = String(eligible); value["excluded_files"] = String(2 - eligible)
        value["examined_files"] = String(min(step, eligible)); value["scanned_files"] = String(min(step, eligible))
        value["pending_files"] = String(max(0, eligible - step))
        if !pathScope.isWorkspace {
            value["scope_path_kind"] = pathScope.kind.rawValue
            value["scope_path"] = pathScope.path; value["scope_path_hex"] = pathScope.pathHex
        }
        if let hitPath, total > 0 { value["hits"] = (1...min(total, 64)).map { hit($0, path: hitPath) } }
        return value
    }
    private static func setupScoped(_ scope: AtlasSearchFileScope, eligible: Int = 1,
                                    pathScope: AtlasSearchPathScope = .workspace, hitPath: String? = nil) throws {
        resetFixture()
        scope.withCStrings { expectScope($0, $1) }
        if !pathScope.isWorkspace { pathScope.withCString { expectPath($0, $1!) } }
        try install(0, openReceipt())
        try install(1, scopedPage("begin", scope: scope, step: 0, eligible: eligible, pathScope: pathScope))
        try install(2, scopedPage("step", scope: scope, step: 1, eligible: eligible,
            total: eligible == 0 ? 0 : 1, running: false, pathScope: pathScope, hitPath: hitPath))
    }
    private static func runScoped(_ scope: AtlasSearchFileScope,
                                  pathScope: AtlasSearchPathScope = .workspace) throws -> AtlasSearchReport {
        try AtlasNativeSearch.runScoped(input: AtlasSearchInput(root: "/project", query: "x", fileScope: scope, pathScope: pathScope),
            cancellation: AtlasSearchCancellation(), access: AtlasSearchAccessLease(nil), progress: { _ in })
    }
    private static func matchingPage(_ command: String, step: Int, total: Int = 0, running: Bool = true,
                                     needle: String = "x", actualText: String = "X",
                                     scope: AtlasSearchFileScope = .all,
                                     pathScope: AtlasSearchPathScope = .workspace) -> [String: Any] {
        let eligible = scope.isAll && pathScope.isWorkspace ? 2 : 1
        let path = pathScope.isWorkspace ? "612e7273" : "7372632f612e7273"
        var value = scopedPage(command, scope: scope, step: step, eligible: eligible, total: total,
            running: running, pathScope: pathScope, hitPath: path)
        value["case_sensitive"] = false; value["mode"] = "unicode-lowercase-literal"; value["needle"] = needle
        value["count_all_matches"] = false; value["match_count_complete"] = !running
        if total > 0 {
            var row = hit(1, path: path)
            row["matched_text"] = actualText
            row["original_range"] = ["start": "0", "end": String(actualText.utf8.count)]
            value["hits"] = [row]
        }
        return value
    }
    private static func setupMatching(needle: String = "x", actualText: String = "X",
                                      scope: AtlasSearchFileScope = .all,
                                      pathScope: AtlasSearchPathScope = .workspace) throws {
        resetFixture(); scope.withCStrings { expectScope($0, $1) }
        if !pathScope.isWorkspace { pathScope.withCString { expectPath($0, $1!) } }
        let extraStep = scope.isAll && pathScope.isWorkspace
        try install(0, openReceipt())
        try install(1, matchingPage("begin", step: 0, needle: needle, actualText: actualText,
            scope: scope, pathScope: pathScope))
        try install(2, matchingPage("step", step: 1, total: 1, running: extraStep,
            needle: needle, actualText: actualText, scope: scope, pathScope: pathScope))
        if extraStep {
            try install(2, matchingPage("step", step: 2, total: 1, running: false,
                needle: needle, actualText: actualText, scope: scope, pathScope: pathScope), index: 1)
        }
    }
    private static func runMatching(needle: String = "x", scope: AtlasSearchFileScope = .all,
                                    pathScope: AtlasSearchPathScope = .workspace) throws -> AtlasSearchReport {
        try AtlasNativeSearch.runScoped(input: AtlasSearchInput(root: "/project", query: needle,
            fileScope: scope, pathScope: pathScope, caseSensitive: false),
            cancellation: AtlasSearchCancellation(), access: AtlasSearchAccessLease(nil), progress: { _ in })
    }
    private static func structurePage(_ command: String, mode: AtlasSearchMode = .symbols,
                                      nameMode: AtlasSearchNameMode = .contains, step: Int,
                                      total: Int = 0, running: Bool = true, complete: Bool = false,
                                      catalogued: Int = 1) -> [String: Any] {
        var value = page(command, step: step, total: total, running: running,
            complete: complete, catalogued: catalogued)
        value["mode"] = mode.wireMode; value["search_strategy"] = "live-structure-scan"
        value["count_all_matches"] = false; value["match_count_complete"] = complete
        value["requested_mode"] = mode.rawValue; value["realized_mode"] = mode.evidence
        value["evidence_level"] = mode.evidence; value["semantic_complete"] = false
        value["name_mode"] = nameMode.rawValue; value["name_case"] = "sensitive"
        value["completeness_basis"] = "admitted-extraction-processing"
        value["structure_file_byte_limit"] = String(mode.atlasStructureFileByteLimit)
        value["unsupported_structure_files"] = "0"; value["extraction_limited_files"] = "0"
        value["structure_metadata_omitted"] = "0"; value["structure_metadata_bytes"] = step == 0 ? "0" : "64"
        if total > 0 {
            var row = hit(1, path: mode == .headings ? "524541444d452e6d64" : "612e7273")
            if mode == .headings {
                row["original_range"] = ["start": "0", "end": "16"]
                row["path"] = ["encoding": "unix-bytes", "hex": "524541444d452e6d64", "display": "README.md"]
            }
            row["structure"] = ["name": "x", "kind": mode == .headings ? "heading" : "function",
                "language": mode == .headings ? "markdown" : "rust", "candidate_id": "1",
                "heading_slug": mode == .headings ? "x" : NSNull(),
                "evidence_range": ["start": "0", "end": "16"],
                "evidence_level": mode.evidence] as [String: Any]
            value["hits"] = [row]
        }
        return value
    }
    private static func setupStructure(_ mode: AtlasSearchMode = .symbols,
                                       nameMode: AtlasSearchNameMode = .contains) throws {
        resetFixture(); expectStructure(mode.code, nameMode.code)
        try install(0, openReceipt())
        try install(1, structurePage("begin-structure", mode: mode, nameMode: nameMode, step: 0))
        try install(2, structurePage("step", mode: mode, nameMode: nameMode, step: 1,
            total: 1, running: false, complete: true))
    }
    private static func runStructure(_ mode: AtlasSearchMode = .symbols,
                                     nameMode: AtlasSearchNameMode = .contains,
                                     cancellation: AtlasSearchCancellation = AtlasSearchCancellation()) throws -> AtlasSearchReport {
        try AtlasNativeSearch.runScoped(input: AtlasSearchInput(root: "/project", query: "x",
            mode: mode, nameMode: nameMode), cancellation: cancellation,
            access: AtlasSearchAccessLease(nil), progress: { _ in })
    }
    @MainActor private static func rejectedStructure(_ label: String, mode: AtlasSearchMode = .headings,
                                                     _ change: () throws -> Void) throws {
        try setupStructure(mode); try change()
        do { _ = try runStructure(mode); preconditionFailure("Accepted invalid structure response: " + label) }
        catch AtlasSearchError.invalidResponse { checks += 1 }
        retired()
        check(fixtureCount(6) == fixtureCount(7), "Structural rejection releases every C allocation: " + label)
    }

    // Last-owner retirement is intentionally asynchronous. Fixture counters are
    // atomic; never reset them until the previous session has actually closed.
    private static func retired() {
        let deadline = Date().addingTimeInterval(5)
        while fixtureCount(5) == 0 && Date() < deadline { Thread.sleep(forTimeInterval: 0.001) }
        precondition(fixtureCount(5) == 1, "Retained source did not retire exactly once")
    }
    @MainActor private static var checks = 0
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message); checks += 1
    }
    @MainActor private static func rejected(_ label: String, _ change: () throws -> Void) throws {
        try setup(); try change()
        do {
            _ = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
            preconditionFailure("Accepted invalid response: " + label)
        } catch AtlasSearchError.invalidResponse {}
        retired()
        check(fixtureCount(5) == 1, label + ": handle closed on rejection")
        check(fixtureCount(6) == fixtureCount(7), label + ": every C allocation released")
    }
    @MainActor private static func discoveryPoliciesBindEveryReceipt() throws {
        for policy in [AtlasDiscoveryPolicy.repositoryRules, .includeExcluded] {
            resetFixture(); expectPolicy(policy.rawValue)
            let receipt = policy == .repositoryRules
                ? policy.wireName + ":" + String(repeating: "a", count: 64) : policy.wireName
            try install(0, openReceipt(receipt))
            var began = page("begin", step: 0, catalogued: 1)
            var finished = page("step", step: 1, total: 1, running: false, complete: true, catalogued: 1)
            began["discovery_policy"] = receipt; finished["discovery_policy"] = receipt
            try install(1, began); try install(2, finished)
            let progress = Reports()
            var report: AtlasSearchReport? = try AtlasNativeSearch.runScoped(
                input: AtlasSearchInput(root: "/project", query: "x", discoveryPolicy: policy),
                cancellation: AtlasSearchCancellation(), access: AtlasSearchAccessLease(nil), progress: progress.append)
            check(report!.complete && report!.discoveryPolicy == policy
                && progress.values.allSatisfy { $0.discoveryPolicy == policy },
                "Selected discovery policy reaches the ABI, provisional reports and terminal result")
            check(!policy.includesExcluded || report!.summary.contains("Ignored and generated files included"),
                "An expanded search universe is disclosed")
            report = nil; progress.clear(); retired()
            check(fixtureCount(6) == fixtureCount(7), "Policy-scoped responses and retained owner retire")
        }
        for policy in ["all-regular-files/no-rule-files-v1", "repository-rules-v1:", "repository-rules-v1:bad"] {
            try rejected("mismatched or malformed opened discovery policy") { try install(0, openReceipt(policy)) }
            check(fixtureCount(2) == 0, "Wrong-policy open is rejected before source-search admission")
        }
        for openedOwner in ["0", "01", "invalid", "18446744073709551616", "12"] {
            try rejected("opened owner must match the allocated native owner") {
                var opened = openReceipt(); opened["owner"] = openedOwner
                var began = page("begin", step: 0); began["owner"] = openedOwner
                try install(0, opened); try install(1, began)
            }
            check(fixtureCount(2) == 0 && fixtureCount(3) == 0,
                "Malformed or consistently forged positive owner is refused before search admission or source steps")
        }
        for phase in [UInt32(1), 2, 3] {
            try rejected("policy changed during search phase \(phase)") {
                var changed = phase == 1 ? page("begin", step: 0)
                    : page(phase == 2 ? "step" : "page", step: 1, total: 70, start: phase == 3 ? 64 : 0)
                changed["discovery_policy"] = "all-regular-files/no-rule-files-v1"
                try install(phase, changed)
            }
        }
        try rejected("frozen rule fingerprint changed between open and query") {
            try install(0, openReceipt("repository-rules-v1:" + String(repeating: "a", count: 64)))
        }
        try setup(); expectPolicy(1)
        do {
            _ = try AtlasNativeSearch.runScoped(
                input: AtlasSearchInput(root: "/project", query: "x", discoveryPolicy: .includeExcluded),
                cancellation: AtlasSearchCancellation(), access: AtlasSearchAccessLease(nil), progress: { _ in })
            preconditionFailure("Include-excluded query accepted narrower rules receipt")
        } catch AtlasSearchError.invalidResponse { checks += 1 }
        retired()
        check(fixtureCount(2) == 0 && fixtureCount(6) == fixtureCount(7),
            "Expanded requests never silently fall back to repository rules")
    }

    private static func catalogEcho(_ original: [String: Any], scope: AtlasSearchCatalogScope) -> [String: Any] {
        var value = original
        value["discovery_scope"] = scope.kind.rawValue
        value["discovery_directory"] = scope.kind == .directory
            ? ["encoding": "unix-bytes", "hex": scope.pathHex, "display": "selected folder"] as Any : NSNull()
        value["discovery_file"] = scope.kind == .file
            ? ["encoding": "unix-bytes", "hex": scope.pathHex, "display": "selected file"] as Any : NSNull()
        value["path_basis"] = "project-root"
        value["discovery_excluded_entries"] = "0"; value["discovery_unavailable_entries"] = "0"
        value["discovery_symlinks_not_followed"] = "0"; value["discovery_special_objects"] = "0"
        return value
    }
    private static func directoryPage(_ command: String, step: Int, total: Int, running: Bool,
                                     scope: AtlasSearchCatalogScope, path: AtlasSearchPathScope,
                                     policy: AtlasDiscoveryPolicy) -> [String: Any] {
        let eligible = path.isVisible ? 1 : 2
        let member = scope.directory + Array("/a.rs".utf8)
        let memberHex = member.map { String(format: "%02x", $0) }.joined()
        var value = scopedPage(command, scope: .all, step: step, eligible: eligible, total: total,
            running: running, pathScope: path, hitPath: memberHex)
        if command == "page" {
            value["hits"] = (65...total).map { hit($0, path: memberHex) }
            value["next_offset"] = NSNull()
        }
        if path.isVisible {
            value["scope_path"] = ""; value["scope_path_hex"] = ""
            value["scope_region_sha256"] = path.visibleIdentity
            value["scope_region_files"] = String(path.visibleCount ?? 0)
            value["scope_region_path_bytes"] = String(member.count)
            value["scope_region_catalogued_files"] = "1"; value["scope_region_missing_files"] = "0"
        }
        value["discovery_policy"] = policy.wireName
        return catalogEcho(value, scope: scope)
    }
    private static func directoryImport(_ scope: AtlasSearchCatalogScope) -> [String: Any] {
        let member = scope.directory + Array("/a.rs".utf8)
        let rawPath: [String: Any] = ["encoding": "unix-bytes",
            "hex": member.map { String(format: "%02x", $0) }.joined(), "display": "a.rs"]
        var imported = importReply(), reader = imported["reader"] as! [String: Any]
        imported["path"] = rawPath; reader["path"] = rawPath; imported["reader"] = reader
        return catalogEcho(imported, scope: scope)
    }
    private static func setupDirectory(_ scope: AtlasSearchCatalogScope, path: AtlasSearchPathScope,
                                       policy: AtlasDiscoveryPolicy = .repositoryRules, rows: Int = 1) throws {
        resetFixture(); expectPolicy(policy.rawValue)
        scope.directory.withUnsafeBufferPointer { expectDirectory($0.baseAddress, UInt64($0.count)) }
        path.withCString { expectPath($0, $1!) }
        try install(0, catalogEcho(openReceipt(policy.wireName), scope: scope))
        try install(1, directoryPage("begin", step: 0, total: 0, running: true, scope: scope, path: path, policy: policy))
        try install(2, directoryPage("step", step: 1, total: rows, running: !path.isVisible, scope: scope, path: path, policy: policy))
        if rows > 64 {
            try install(3, directoryPage("page", step: 1, total: rows, running: !path.isVisible, scope: scope, path: path, policy: policy))
        }
        if !path.isVisible {
            try install(2, directoryPage("step", step: 2, total: rows, running: false,
                scope: scope, path: path, policy: policy), index: 1)
        }
        try install(4, directoryImport(scope))
    }
    @MainActor private static func directoryCatalogsBindEveryPhase() throws {
        let folder = try AtlasSearchCatalogScope(directory: Array("zz-late".utf8))
        let path = try AtlasSearchPathScope(kind: .directory, path: "zz-late")
        func run(_ catalog: AtlasSearchCatalogScope, path: AtlasSearchPathScope,
                 policy: AtlasDiscoveryPolicy = .repositoryRules) throws -> AtlasSearchReport {
            try AtlasNativeSearch.runScoped(input: AtlasSearchInput(root: "/project", query: "x", pathScope: path,
                discoveryPolicy: policy, catalogScope: catalog), cancellation: AtlasSearchCancellation(),
                access: AtlasSearchAccessLease(nil), progress: { _ in })
        }
        for policy in [AtlasDiscoveryPolicy.repositoryRules, .includeExcluded] {
            try setupDirectory(folder, path: path, policy: policy, rows: 70)
            var report: AtlasSearchReport? = try run(folder, path: path, policy: policy)
            check(report!.complete && report!.catalogScope == folder && report!.discoveryPolicy == policy
                && report!.hits.count == 70 && report!.hits[0].sourcePath == "zz-late/a.rs",
                "Live folder search keeps independently bounded discovery and root-relative results through every page")
            var target = report!.capture!.target(report!.hits[0])
            report = nil
            check(target!.root == "/project" && target!.path == "zz-late/a.rs",
                "The original project grant and full relative path survive directory-scoped search")
            _ = try target!.openReader(100)
            check(fixtureCount(16) == 1 && fixtureCount(1) == 1 && fixtureCount(4) == 1 && fixtureCount(8) == 1,
                "Folder open, paged query and capture import use one retained directory owner")
            target = nil; retired()
            check(fixtureCount(6) == fixtureCount(7), "Directory search drains every response allocation")
        }
        try setup()
        try install(0, catalogEcho(openReceipt(), scope: .workspace))
        try install(1, catalogEcho(page("begin", step: 0), scope: .workspace))
        try install(2, catalogEcho(page("step", step: 1, total: 70), scope: .workspace))
        try install(3, catalogEcho(page("page", step: 1, total: 70, start: 64), scope: .workspace))
        try install(2, catalogEcho(page("step", step: 2, total: 70, running: false, complete: true), scope: .workspace), index: 1)
        try install(4, catalogEcho(importReply(), scope: .workspace))
        var workspace: AtlasSearchReport? = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
        _ = try workspace!.capture!.target(workspace!.hits[0])!.openReader(100)
        check(workspace!.complete && workspace!.catalogScope.isWorkspace && fixtureCount(16) == 0,
            "Modern workspace receipts preserve default opening, pagination and retained activation")
        workspace = nil; retired()
        for phase in [UInt32(0), 1, 2, 3, 4] {
            try setupDirectory(folder, path: path, rows: 70)
            var corrupt: [String: Any]
            if phase == 0 { corrupt = catalogEcho(openReceipt(), scope: folder) }
            else if phase == 4 { corrupt = directoryImport(folder) }
            else {
                corrupt = directoryPage(phase == 1 ? "begin" : phase == 2 ? "step" : "page",
                    step: phase == 1 ? 0 : 1, total: phase == 1 ? 0 : 70, running: true,
                    scope: folder, path: path, policy: .repositoryRules)
            }
            corrupt["discovery_directory"] = ["encoding": "unix-bytes", "hex": "6f74686572", "display": "zz-late"]
            try install(phase, corrupt)
            do {
                let result = try run(folder, path: path)
                if phase == 4 { _ = try result.capture!.target(result.hits[0])!.openReader(100) }
                preconditionFailure("Live receipt changed catalog directory at phase " + String(phase))
            } catch AtlasSearchError.invalidResponse { checks += 1 }
            retired()
            if phase == 0 { check(fixtureCount(2) == 0, "A mismatched directory open cannot admit source search") }
            check(fixtureCount(6) == fixtureCount(7), "Rejected directory receipts preserve allocation balance")
        }
        for phase in [UInt32(0), 1, 2, 3, 4] {
            for defect in ["partial", "all-null"] {
                try setup()
                var changed = phase == 0 ? openReceipt() : phase == 4 ? importReply()
                    : page(phase == 1 ? "begin" : phase == 2 ? "step" : "page",
                        step: phase == 1 ? 0 : 1, total: phase == 1 ? 0 : 70, start: phase == 3 ? 64 : 0)
                changed["discovery_directory"] = NSNull()
                if defect == "all-null" {
                    changed["discovery_scope"] = NSNull(); changed["path_basis"] = NSNull()
                }
                try install(phase, changed)
                do {
                    let result = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
                    if phase == 4 { _ = try result.capture!.target(result.hits[0])!.openReader(100) }
                    preconditionFailure("Legacy workspace accepted a partial or null-only discovery receipt")
                } catch AtlasSearchError.invalidResponse { checks += 1 }
                retired()
            }
        }
        #if canImport(CryptoKit)
        let raw = try AtlasSearchCatalogScope(directory: [122, 255, 92, 45])
        let visible = try AtlasSearchPathScope(visiblePaths: [raw.directory + Array("/a.rs".utf8)])
        try setupDirectory(raw, path: visible)
        var visibleReport: AtlasSearchReport? = try run(raw, path: visible)
        check(visibleReport!.complete && visibleReport!.catalogScope == raw
            && visibleReport!.pathScope == visible && visibleReport!.hits[0].sourcePath == nil
            && fixtureCount(16) == 1, "Visible search uses its accepted raw directory without a lossy path or root-prefix catalog")
        visibleReport = nil; retired()
        #endif
    }

    private static func exactFilePage(_ command: String, step: Int, total: Int = 0, start: Int = 0,
                                      scope: AtlasSearchCatalogScope, path: AtlasSearchPathScope,
                                      policy: AtlasDiscoveryPolicy = .repositoryRules, excluded: Bool = false) -> [String: Any] {
        let files = excluded ? 0 : 1
        var value = page(command, step: step, total: total, start: start,
            running: step == 0, complete: step > 0 && !excluded, catalogued: files)
        value["search_scope"] = "all"; value["scope_extensions"] = ""; value["scope_filenames"] = ""
        value["eligible_files"] = String(files); value["excluded_files"] = "0"
        value["scope_path_kind"] = path.kind.rawValue
        value["scope_path"] = path.isVisible ? "" : path.path; value["scope_path_hex"] = path.pathHex
        if path.isVisible {
            value["scope_region_sha256"] = path.visibleIdentity
            value["scope_region_files"] = "1"; value["scope_region_path_bytes"] = String(scope.file.count)
            value["scope_region_catalogued_files"] = String(files)
            value["scope_region_missing_files"] = String(1 - files)
        }
        let rows = value["hits"] as! [[String: Any]]
        value["hits"] = rows.map { row in hit(Int(row["hit_id"] as! String)!, path: scope.pathHex) }
        value["discovery_policy"] = policy.wireName
        value["discovery_complete"] = !excluded
        value = catalogEcho(value, scope: scope)
        value["discovery_excluded_entries"] = excluded ? "1" : "0"
        return value
    }
    private static func exactFileImport(_ scope: AtlasSearchCatalogScope) -> [String: Any] {
        let path: [String: Any] = ["encoding": "unix-bytes", "hex": scope.pathHex, "display": "selected file"]
        var imported = importReply(), reader = imported["reader"] as! [String: Any]
        imported["path"] = path; reader["path"] = path; imported["reader"] = reader
        return catalogEcho(imported, scope: scope)
    }
    private static func setupExactFile(_ scope: AtlasSearchCatalogScope, path: AtlasSearchPathScope,
                                      policy: AtlasDiscoveryPolicy = .repositoryRules, rows: Int = 70,
                                      excluded: Bool = false) throws {
        resetFixture(); expectPolicy(policy.rawValue)
        scope.file.withUnsafeBufferPointer { expectFile($0.baseAddress, UInt64($0.count)) }
        path.withCString { expectPath($0, $1!) }
        var opened = catalogEcho(openReceipt(policy.wireName), scope: scope)
        opened["discovery_excluded_entries"] = excluded ? "1" : "0"
        try install(0, opened)
        try install(1, exactFilePage("begin", step: 0, scope: scope, path: path, policy: policy, excluded: excluded))
        try install(2, exactFilePage("step", step: 1, total: excluded ? 0 : rows,
            scope: scope, path: path, policy: policy, excluded: excluded))
        if rows > 64 && !excluded {
            try install(3, exactFilePage("page", step: 1, total: rows, start: 64, scope: scope, path: path, policy: policy))
        }
        try install(4, exactFileImport(scope))
    }
    @MainActor private static func exactFileCatalogsBindEveryPhase() throws {
        let file = try AtlasSearchCatalogScope(file: Array("zz-late/chosen.rs".utf8))
        let path = try AtlasSearchPathScope(kind: .file, path: "zz-late/chosen.rs")
        func run(_ catalog: AtlasSearchCatalogScope? = nil, path selection: AtlasSearchPathScope? = nil,
                 policy: AtlasDiscoveryPolicy = .repositoryRules) throws -> AtlasSearchReport {
            try AtlasNativeSearch.runScoped(input: AtlasSearchInput(root: "/project", query: "x", pathScope: selection ?? path,
                discoveryPolicy: policy, catalogScope: catalog ?? file), cancellation: AtlasSearchCancellation(),
                access: AtlasSearchAccessLease(nil), progress: { _ in })
        }
        for policy in [AtlasDiscoveryPolicy.repositoryRules, .includeExcluded] {
            try setupExactFile(file, path: path, policy: policy)
            var report: AtlasSearchReport? = try run(policy: policy)
            check(report!.complete && report!.catalogScope == file && report!.hits.count == 70
                && report!.progress?.cataloguedFiles == 1 && report!.discoveryCoverage?.excludedEntries == 0,
                "Live exact-file search uses its independently discovered one-member catalog")
            var target = report!.capture!.target(report!.hits[0])
            report = nil
            check(target!.root == "/project" && target!.path == "zz-late/chosen.rs",
                "File discovery retains the original project grant and full relative source path")
            _ = try target!.openReader(100)
            check(fixtureCount(17) == 1 && fixtureCount(16) == 0 && fixtureCount(1) == 1
                && fixtureCount(4) == 1 && fixtureCount(8) == 1,
                "File open, paging and source activation never substitute a parent-directory opener")
            target = nil; retired()
            check(fixtureCount(6) == fixtureCount(7), "Exact-file search balances response allocations")
        }
        for phase in [UInt32(0), 1, 2, 3, 4] {
            try setupExactFile(file, path: path)
            var wrong: [String: Any]
            if phase == 0 { wrong = catalogEcho(openReceipt(), scope: file) }
            else if phase == 4 { wrong = exactFileImport(file) }
            else {
                wrong = exactFilePage(phase == 1 ? "begin" : phase == 2 ? "step" : "page",
                    step: phase == 1 ? 0 : 1, total: phase == 1 ? 0 : 70, start: phase == 3 ? 64 : 0,
                    scope: file, path: path)
            }
            wrong["discovery_file"] = ["encoding": "unix-bytes", "hex": "6f746865722e7273", "display": "chosen.rs"]
            try install(phase, wrong)
            do {
                let result = try run()
                if phase == 4 { _ = try result.capture!.target(result.hits[0])!.openReader(100) }
                preconditionFailure("A changed file receipt entered a live source phase")
            } catch AtlasSearchError.invalidResponse { checks += 1 }
            retired()
            if phase == 0 { check(fixtureCount(2) == 0, "A wrong selected file cannot begin source reads") }
            check(fixtureCount(6) == fixtureCount(7), "Rejected file receipts release every response")
        }
        for missing in ["discovery_file", "discovery_directory", "discovery_excluded_entries"] {
            try setupExactFile(file, path: path)
            var opened = catalogEcho(openReceipt(), scope: file); opened.removeValue(forKey: missing)
            try install(0, opened)
            do { _ = try run(); preconditionFailure("Incomplete file discovery was accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            retired()
            check(fixtureCount(2) == 0, "File discovery refuses absent identity or coverage before query admission")
        }
        try setupExactFile(file, path: path, excluded: true)
        var excluded: AtlasSearchReport? = try run()
        check(!excluded!.complete && excluded!.hits.isEmpty && excluded!.excludedFiles == 0
            && excluded!.discoveryCoverage?.excludedEntries == 1
            && excluded!.summary.contains("excluded from discovery"),
            "Excluded selected files are explained separately from type filters and complete no-match results")
        excluded = nil; retired()
        try setupExactFile(file, path: path); cancelStep()
        do { _ = try run(); preconditionFailure("Exact-file capture ignored cooperative cancellation") }
        catch AtlasSearchError.canceled { checks += 1 }
        retired()
        check(fixtureCount(6) == fixtureCount(7), "Canceled exact-file capture releases allocations")
        #if canImport(CryptoKit)
        let raw = try AtlasSearchCatalogScope(file: [122, 255, 92, 45, 47, 97, 46, 114, 115])
        let visible = try AtlasSearchPathScope(visiblePaths: [raw.file])
        try setupExactFile(raw, path: visible, rows: 1)
        var opaque: AtlasSearchReport? = try run(raw, path: visible)
        check(opaque!.complete && opaque!.catalogScope == raw && opaque!.hits[0].sourcePath == nil
            && fixtureCount(17) == 1, "Exact-file C marshaling preserves raw names without requiring UTF8")
        opaque = nil; retired()
        #endif
    }

    @MainActor static func main() throws {
        resetFixture()
        let windowInput = try AtlasSearchInput(root: "/project", query: "x")
        let windowSource = AtlasSearchResultWindowSource(index: UUID(), input: windowInput, countAllMatches: true)
        let windowRequest = try AtlasSearchInput(root: "/project", query: "x",
            resultWindow: .init(source: windowSource, offset: 1000))
        do {
            _ = try AtlasNativeSearch.runScoped(input: windowRequest, cancellation: AtlasSearchCancellation(),
                access: AtlasSearchAccessLease(nil), progress: { _ in })
            preconditionFailure("A captured result window became a fresh live first-page query")
        } catch AtlasSearchError.unavailable { checks += 1 }
        check(fixtureCount(0) == 0 && fixtureCount(1) == 0 && fixtureCount(2) == 0,
            "Live search refuses retained-index window requests before allocation, discovery or query admission")
        try directoryCatalogsBindEveryPhase()
        try exactFileCatalogsBindEveryPhase()
        try discoveryPoliciesBindEveryReceipt()
        for caseSensitive in [true, false] {
            resetFixture(); expectCountedCase(caseSensitive ? 1 : 0)
            try install(0, openReceipt())
            try install(1, countedPage("begin", step: 0, caseSensitive: caseSensitive))
            try install(2, countedPage("step", step: 1, caseSensitive: caseSensitive))
            for (index, start) in stride(from: 64, to: 1000, by: 128).enumerated() {
                try install(3, countedPage("page", step: 1, start: start, caseSensitive: caseSensitive),
                    index: UInt32(index))
            }
            try install(2, countedPage("step", step: 2, caseSensitive: caseSensitive), index: 1)
            var counted: AtlasSearchReport? = try AtlasNativeSearch.runScoped(
                input: AtlasSearchInput(root: "/project", query: "x", caseSensitive: caseSensitive, countAllMatches: true),
                cancellation: AtlasSearchCancellation(), access: AtlasSearchAccessLease(nil), progress: { _ in })
            check(counted!.complete && counted!.matchCountComplete && counted!.truncated
                && counted!.matchesSeen == 1200 && counted!.hits.count == 1000,
                "Both matching modes finish counting beyond the bounded retained pages")
            check(counted!.summary.contains("1200") && counted!.summary.contains("first 1000 retained"),
                "Live totals distinguish counted occurrences from navigable rows")
            check(fixtureCount(14) == 1 && fixtureCount(12) == 0 && fixtureCount(4) == 8,
                "Explicit counting routes through the counted ABI and fetches only bounded retained rows")
            counted = nil; retired()
            check(fixtureCount(6) == fixtureCount(7), "Counted pages release every owned C response")
        }
        do {
            resetFixture()
            try install(0, openReceipt())
            var began = page("begin", step: 0); began["count_all_matches"] = false
            var finished = page("step", step: 1, total: 1, running: false, complete: true, catalogued: 1)
            finished["count_all_matches"] = false; began["catalogued_files"] = "1"; began["pending_files"] = "1"
            try install(1, began); try install(2, finished)
            var bounded: AtlasSearchReport? = try AtlasNativeSearch.runScoped(
                input: AtlasSearchInput(root: "/project", query: "x", countAllMatches: false),
                cancellation: AtlasSearchCancellation(), access: AtlasSearchAccessLease(nil), progress: { _ in })
            check(bounded!.complete && !bounded!.countAllMatches && fixtureCount(14) == 0,
                "Turning counting off explicitly preserves the bounded exact-text route")
            bounded = nil; retired()
        }
        try setup()
        let progress = Reports()
        var terminal: AtlasSearchReport? = try AtlasNativeSearch.runProgressive(root: "/project", query: "x",
            cancellation: AtlasSearchCancellation(), progress: progress.append)
        check(terminal!.complete && !terminal!.isInProgress && terminal!.hits.count == 70, "Complete paginated result")
        check(terminal!.hits.map(\.id) == Array(0..<70), "Stable contiguous occurrence IDs")
        check(progress.values.contains { $0.isInProgress && $0.hits.count == 70 }, "Matches publish before final file")
        check(progress.values.allSatisfy { !$0.complete && $0.streamID == terminal!.streamID }, "Coverage and identity remain independent")
        check(fixtureCount(1) == 1 && fixtureCount(2) == 1 && fixtureCount(3) == 2
              && fixtureCount(14) == 1,
              "One discovery, one counted admission, one step per file")
        check(fixtureCount(4) == 1, "Later progress does not refetch already retained pages")
        let firstID = terminal!.streamID
        var target = terminal!.capture?.target(terminal!.hits[0])
        check(target != nil && fixtureCount(5) == 0, "Terminal report retains an importable exact source")
        terminal = nil; progress.clear()
        check(fixtureCount(5) == 0, "Selected target outlives all report pages")
        let info = try target!.openReader(100)
        check(info.contains("utf16le") && fixtureCount(8) == 1, "Actual worker capability imports captured UTF-16 metadata")
        check(fixtureCount(1) == 1 && fixtureCount(3) == 2, "Activation neither rediscovers nor repeats source search")
        target = nil; retired()
        check(fixtureCount(5) == 1 && fixtureCount(6) == fixtureCount(7), "All retained/C resources retired")

        try setup()
        var again: AtlasSearchReport? = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
        check(again!.streamID != firstID, "Repeated query starts a new stream")
        again = nil; retired()
        try setup()
        var partial = page("step", step: 2, total: 70, running: false)
        partial["unavailable_files"] = "1"
        try install(2, partial, index: 1)
        var incomplete: AtlasSearchReport? = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
        check(!incomplete!.complete && !incomplete!.isInProgress && incomplete!.unavailableFiles == 1,
              "Finished with unavailable files is partial, not running or exhaustive")
        incomplete = nil; retired()

        try setup()
        let flag = AtlasSearchCancellation()
        do {
            _ = try AtlasNativeSearch.runProgressive(root: "/project", query: "x", cancellation: flag) { report in
                if !report.hits.isEmpty { flag.cancel() }
            }
            preconditionFailure("Cancellation ignored")
        } catch AtlasSearchError.canceled {}
        retired()
        check(fixtureCount(3) == 1 && fixtureCount(5) == 1, "Cancellation between files prevents next read")
        check(fixtureCount(6) == fixtureCount(7), "Cancel after publication releases all strings")
        try setup(); cancelStep()
        do {
            _ = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
            preconditionFailure("FFI cancellation ignored")
        } catch AtlasSearchError.canceled {}
        retired()
        check(fixtureCount(5) == 1 && fixtureCount(6) == fixtureCount(7), "Cancellation racing a returned C string releases it")
        try setup()
        let preCanceled = AtlasSearchCancellation(); preCanceled.cancel()
        do {
            _ = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: preCanceled)
            preconditionFailure("Pre-canceled work admitted")
        } catch AtlasSearchError.canceled {}
        check(fixtureCount(0) == 0, "Pre-canceled work never creates a session")
        try setup(); refuseCreate()
        do {
            _ = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
            preconditionFailure("Refused handle admitted")
        } catch AtlasSearchError.unavailable {}
        check(fixtureCount(5) == 0 && fixtureCount(1) == 0, "Refused handle is not opened or closed")
        try setup(); try install(0, ["status": "error"])
        do {
            _ = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
            preconditionFailure("Failed discovery admitted")
        } catch AtlasSearchError.unavailable {}
        retired()
        check(fixtureCount(5) == 1 && fixtureCount(2) == 0, "Failed discovery closes before querying")

        for (key, value) in [("owner", "12"), ("source_manifest", "2"), ("layout_revision", "2"),
                             ("query_generation", "2"), ("needle", "y"), ("step_count", "01"),
                             ("mode", "regex"), ("search_strategy", "unknown")] {
            try rejected(key) {
                var bad = page("step", step: 1, total: 70); bad[key] = value
                try install(2, bad)
            }
        }
        try rejected("count policy") {
            var bad = page("step", step: 1, total: 70); bad["count_all_matches"] = false
            try install(2, bad)
        }
        try rejected("count completeness") {
            var bad = page("step", step: 1, total: 70); bad["match_count_complete"] = true
            try install(2, bad)
        }
        for stop in ["file-limit", "source-byte-limit", "verification-byte-limit"] {
            try rejected("complete counted result with " + stop) {
                var bad = page("step", step: 2, total: 70, running: false, complete: true)
                bad["stop_reason"] = stop
                try install(2, bad, index: 1)
            }
        }
        try rejected("premature complete") {
            try install(2, page("step", step: 1, total: 70, complete: true))
        }
        try rejected("counter regression") {
            try install(2, page("step", step: 2, total: 69, running: false, complete: true), index: 1)
        }
        try rejected("rewinding cursor") {
            var bad = page("step", step: 1, total: 70); bad["next_offset"] = "0"
            try install(2, bad)
        }
        try rejected("page from different coverage") {
            var bad = page("page", step: 1, total: 70, start: 64); bad["matches_seen"] = "71"
            try install(3, bad)
        }
        try rejected("changed previous witness") {
            var bad = page("step", step: 2, total: 70, running: false, complete: true)
            var rows = bad["hits"] as! [[String: Any]]
            rows[0]["capture_sha256"] = String(repeating: "b", count: 64); bad["hits"] = rows
            try install(2, bad, index: 1)
        }
        for path in ["2f61", "2e2e2f61", "612f2e2f62", "612f2f62", "6100", "612F62", "f"] {
            try rejected("invalid raw path " + path) {
                var bad = page("step", step: 1, total: 1); bad["hits"] = [hit(1, path: path)]
                try install(2, bad)
            }
        }
        for (key, value) in [("capture_sha256", "not-a-digest"), ("capture_byte_length", "0"),
                             ("source_revision", "0"), ("hit_id", "2"), ("file_id", "01")] {
            try rejected(key) {
                var bad = page("step", step: 1, total: 1), row = hit(1)
                row[key] = value; bad["hits"] = [row]; try install(2, bad)
            }
        }
        try setup()
        var nonUTF8 = page("step", step: 1, total: 1)
        nonUTF8["hits"] = [hit(1, path: "ff2e7273")]
        try install(2, nonUTF8)
        var nonUTF8Final = page("step", step: 2, total: 1, running: false, complete: true)
        nonUTF8Final["hits"] = [hit(1, path: "ff2e7273")]
        try install(2, nonUTF8Final, index: 1)
        var raw: AtlasSearchReport? = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
        check(raw!.hits.count == 1 && raw!.hits[0].sourcePath == nil, "Non-UTF-8 path remains visible without fabricated opener")
        check(raw!.hits[0].captureSHA256 != nil, "Raw-name row retains its source witness")
        check(raw!.capture?.target(raw!.hits[0]) == nil, "Unsupported native path does not acquire a false text-path opener")
        raw = nil; retired()
        let rustScope = try AtlasFileScope.rust.searchScope()
        try setupScoped(rustScope)
        var scoped: AtlasSearchReport? = try runScoped(rustScope)
        check(scoped!.complete && scoped!.hits.count == 1 && scoped!.fileScope == rustScope, "Scoped native route returns only admitted witnesses")
        check(scoped!.progress?.cataloguedFiles == 1 && scoped!.excludedFiles == 1, "Progress denominator is eligible files")
        check(fixtureCount(9) == 1 && fixtureCount(3) == 1, "Scoped C admission receives both canonical lists")
        scoped = nil; retired()
        let emptyScope = try AtlasSearchFileScope(extensions: [])
        try setupScoped(emptyScope, eligible: 0)
        let empty = try runScoped(emptyScope)
        check(empty.complete && empty.hits.isEmpty && empty.progress?.examinedFiles == 0
            && empty.progress?.cataloguedFiles == 0 && empty.excludedFiles == 2, "Explicit empty scope completes without widening")
        retired()
        for defect in ["missing", "predicate", "denominator", "excluded-hit"] {
            try setupScoped(rustScope)
            if defect == "missing" {
                try install(1, page("begin", step: 0))
            } else {
                var bad = scopedPage("step", scope: rustScope, step: 1, eligible: 1, total: 1, running: false)
                if defect == "predicate" { bad["scope_extensions"] = "py" }
                if defect == "denominator" { bad["excluded_files"] = "0" }
                if defect == "excluded-hit" { bad["hits"] = [hit(1, path: "612e747874")] }
                try install(2, bad)
            }
            do { _ = try runScoped(rustScope); preconditionFailure("Accepted scoped defect: \(defect)") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            retired()
            check(fixtureCount(6) == fixtureCount(7), "Scope rejection releases every C allocation")
        }
        let directory = try AtlasSearchPathScope(kind: .directory, path: "src")
        let file = try AtlasSearchPathScope(kind: .file, path: "src/a.rs")
        for pathScope in [directory, file] {
            try setupScoped(.all, pathScope: pathScope, hitPath: "7372632f612e7273")
            var result: AtlasSearchReport? = try runScoped(.all, pathScope: pathScope)
            check(result!.complete && result!.hits.count == 1 && result!.hits[0].sourcePath == "src/a.rs"
                && result!.pathScope == pathScope, "Exact requested location survives the C worker")
            check(result!.progress?.cataloguedFiles == 1 && result!.excludedFiles == 1,
                "Path-only scope uses eligible coverage even when all extensions are included")
            check(fixtureCount(10) == 1 && fixtureCount(9) == 0, "Location scope uses its dedicated C admission")
            result = nil; retired()
        }
        try setupScoped(rustScope, pathScope: directory, hitPath: "7372632fff2e7273")
        var rawChild: AtlasSearchReport? = try runScoped(rustScope, pathScope: directory)
        check(rawChild!.hits.count == 1 && rawChild!.hits[0].sourcePath == nil
            && rawChild!.capture?.target(rawChild!.hits[0]) == nil,
            "An opaque descendant is searchable without a fabricated Unicode opener")
        rawChild = nil; retired()
        try setupScoped(.all, eligible: 0, pathScope: file)
        let noFile = try runScoped(.all, pathScope: file)
        check(noFile.complete && noFile.hits.isEmpty && noFile.excludedFiles == 2
            && noFile.pathScope == file, "An absent exact file remains an empty scope")
        retired()
        for defect in ["missing", "kind", "path", "hex", "sibling-prefix", "outside-root", "range"] {
            try setupScoped(.all, pathScope: directory, hitPath: "7372632f612e7273")
            var bad = scopedPage("step", scope: .all, step: 1, eligible: 1, total: 1, running: false,
                pathScope: directory, hitPath: "7372632f612e7273")
            if defect == "missing" {
                for key in ["scope_path_kind", "scope_path", "scope_path_hex"] { bad.removeValue(forKey: key) }
            } else if defect == "kind" { bad["scope_path_kind"] = "file" }
            else if defect == "path" { bad["scope_path"] = "src2" }
            else if defect == "hex" { bad["scope_path_hex"] = "00" }
            else if defect == "sibling-prefix" { bad["hits"] = [hit(1, path: "737263322f612e7273")] }
            else if defect == "outside-root" { bad["hits"] = [hit(1, path: "7372632f2e2e2f612e7273")] }
            else {
                var row = hit(1, path: "7372632f612e7273")
                row["original_range"] = ["start": "0", "end": "1001"]; bad["hits"] = [row]
            }
            try install(2, bad)
            do { _ = try runScoped(.all, pathScope: directory); preconditionFailure("Path-scope defect accepted: \(defect)") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            retired()
            check(fixtureCount(6) == fixtureCount(7), "Path rejection releases every C allocation")
        }
        let composed = try AtlasSearchPathScope(kind: .directory, path: "café")
        let decomposed = try AtlasSearchPathScope(kind: .directory, path: "cafe\u{301}")
        for defect in ["echo", "row"] {
            try setupScoped(.all, pathScope: composed, hitPath: composed.pathHex + "2f612e7273")
            var bad = scopedPage("step", scope: .all, step: 1, eligible: 1, total: 1, running: false,
                pathScope: composed, hitPath: composed.pathHex + "2f612e7273")
            if defect == "echo" { bad["scope_path"] = decomposed.path }
            else { bad["hits"] = [hit(1, path: decomposed.pathHex + "2f612e7273")] }
            try install(2, bad)
            do { _ = try runScoped(.all, pathScope: composed); preconditionFailure("Normalized path scope accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            retired()
        }
        try setupScoped(.all, pathScope: directory, hitPath: "7372632f612e7273"); cancelStep()
        do { _ = try runScoped(.all, pathScope: directory); preconditionFailure("Path-scoped cancellation ignored") }
        catch AtlasSearchError.canceled { checks += 1 }
        retired()
        check(fixtureCount(6) == fixtureCount(7), "Racing path-scoped cancellation releases every C allocation")
        for (needle, actualText) in [("x", "X"), ("i\u{307}", "İ"), ("k", "K")] {
            try setupMatching(needle: needle, actualText: actualText)
            var result: AtlasSearchReport? = try runMatching(needle: needle)
            check(result!.complete && !result!.caseSensitive && result!.hits.count == 1
                && result!.hits[0].actualText?.utf8.elementsEqual(actualText.utf8) == true,
                "Case-insensitive receipt retains the engine's actual Unicode spelling")
            check(result!.hits[0].end == UInt64(actualText.utf8.count),
                "Source spans do not inherit the transformed query's byte length")
            check(result!.capture?.target(result!.hits[0])?.needle.utf8.elementsEqual(actualText.utf8) == true
                && result!.capture?.target(result!.hits[0])?.caseSensitive == false,
                "Retained reader receives exact source spelling instead of the insensitive query")
            check(fixtureCount(12) == 1 && fixtureCount(9) == 0 && fixtureCount(10) == 0,
                "Insensitive request uses matching admission with all scope fields")
            check(result!.summary.contains("case-insensitive") && result!.summary.contains("Unicode lowercase"),
                "Results disclose their matching semantics")
            result = nil; retired()
        }
        try setupMatching(scope: rustScope, pathScope: directory)
        var locationMatch: AtlasSearchReport? = try runMatching(scope: rustScope, pathScope: directory)
        check(locationMatch!.fileScope == rustScope && locationMatch!.pathScope == directory
            && !locationMatch!.caseSensitive && locationMatch!.excludedFiles == 1,
            "Match case combines with exact location and file-type restrictions")
        locationMatch = nil; retired()
        for defect in ["missing-case", "case", "mode", "missing-text", "empty-text", "nul-text", "oversized-text"] {
            try setupMatching()
            var bad = matchingPage("step", step: 1, total: 1)
            if defect == "missing-case" { bad.removeValue(forKey: "case_sensitive") }
            else if defect == "case" { bad["case_sensitive"] = true }
            else if defect == "mode" { bad["mode"] = "exact-decoded-literal" }
            else {
                var rows = bad["hits"] as! [[String: Any]]
                if defect == "missing-text" { rows[0].removeValue(forKey: "matched_text") }
                if defect == "empty-text" { rows[0]["matched_text"] = "" }
                if defect == "nul-text" { rows[0]["matched_text"] = "x\0y" }
                if defect == "oversized-text" { rows[0]["matched_text"] = String(repeating: "X", count: 4097) }
                bad["hits"] = rows
            }
            try install(2, bad)
            do { _ = try runMatching(); preconditionFailure("Invalid matching receipt accepted: " + defect) }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            retired()
            check(fixtureCount(6) == fixtureCount(7), "Matching refusal releases every C response")
        }
        try setupMatching(needle: "é", actualText: "é")
        var changedSpelling = matchingPage("step", step: 2, total: 1, running: false, needle: "é", actualText: "é")
        var changedRows = changedSpelling["hits"] as! [[String: Any]]
        changedRows[0]["matched_text"] = "e\u{301}"; changedSpelling["hits"] = changedRows
        try install(2, changedSpelling, index: 1)
        do { _ = try runMatching(needle: "é"); preconditionFailure("Canonical alias changed a repeated hit witness") }
        catch AtlasSearchError.invalidResponse { checks += 1 }
        retired()
        try rejected("case receipt contradicts sensitive request") {
            var bad = page("step", step: 1, total: 70); bad["case_sensitive"] = false
            try install(2, bad)
        }
        try rejected("sensitive spelling contradicts literal query") {
            var bad = page("step", step: 1, total: 70)
            var rows = bad["hits"] as! [[String: Any]]; rows[0]["matched_text"] = "X"; bad["hits"] = rows
            try install(2, bad)
        }
        // Changing from an exact/prefix name query back to literal text must
        // normalize the inactive name control before coordinator admission.
        for nameMode in AtlasSearchNameMode.allCases {
            let literal = try AtlasSearchInput(root: "/project", query: "x",
                mode: .text, nameMode: nameMode)
            check(literal.mode == .text && literal.nameMode == .contains,
                "Literal request must not inherit a structural name predicate")
        }
        for mode in [AtlasSearchMode.symbols, .headings] {
            for nameMode in AtlasSearchNameMode.allCases {
                try setupStructure(mode, nameMode: nameMode)
                var report: AtlasSearchReport? = try runStructure(mode, nameMode: nameMode)
                check(report!.complete && report!.hits.count == 1 && report!.mode == mode
                    && report!.nameMode == nameMode && report!.structureCoverage?.unsupportedFiles == 0,
                    "Distinct structural admission retains mode and name predicate")
                let hit = report!.hits[0]
                check(hit.structure?.evidence == mode.evidence
                    && report!.capture?.target(hit)?.mode == mode,
                    "Qualified structure evidence remains attached to retained-reader activation")
                check(fixtureCount(11) == 1, "Structural request uses its own C entry")
                if mode == .headings {
                    check(hit.structure?.headingSlug == "x" && hit.start == 0 && hit.end == 16,
                        "Heading syntax span survives independently of the shorter query")
                } else {
                    check(hit.structure?.headingSlug == nil && report!.summary.contains("Heuristic"),
                        "Symbol candidates never claim compiler-level semantics")
                }
                report = nil; retired()
            }
        }
        // Returned heading titles are JSON metadata, not C-string inputs.
        // The shared parser can preserve NUL in a valid bounded title.
        try setupStructure(.headings)
        var nulHeading = structurePage("step", mode: .headings, step: 1,
            total: 1, running: false, complete: true)
        var nulRows = nulHeading["hits"] as! [[String: Any]]
        var nulStructure = nulRows[0]["structure"] as! [String: Any]
        nulStructure["name"] = "x\u{0}y"
        nulRows[0]["structure"] = nulStructure
        nulHeading["hits"] = nulRows
        try install(2, nulHeading)
        var nulReport: AtlasSearchReport? = try runStructure(.headings)
        check(nulReport!.complete && nulReport!.hits[0].structure?.name == "x\u{0}y",
            "Valid returned heading metadata must retain embedded NUL")
        nulReport = nil; retired()
        try rejectedStructure("literal admission command") {
            try install(1, structurePage("begin", mode: .headings, step: 0))
        }
        for (key, badValue) in [
            ("requested_mode", "symbols"), ("realized_mode", "compiler-symbol"),
            ("name_mode", "prefix"), ("evidence_level", "unknown"),
            ("name_case", "insensitive"), ("completeness_basis", "semantic-complete"),
            ("structure_file_byte_limit", "262145"), ("unsupported_structure_files", "1"),
            ("extraction_limited_files", "1"), ("structure_metadata_omitted", "16777217"),
            ("structure_metadata_bytes", "8388609"), ("matches_seen", "1002")
        ] {
            try rejectedStructure(key) {
                var bad = structurePage("step", mode: .headings, step: 1,
                    total: 1, running: false, complete: true)
                bad[key] = badValue; try install(2, bad)
            }
        }
        for missing in ["unsupported_structure_files", "structure_metadata_omitted", "structure_file_byte_limit"] {
            try rejectedStructure("missing " + missing) {
                var bad = structurePage("step", mode: .headings, step: 1,
                    total: 1, running: false, complete: true)
                bad.removeValue(forKey: missing); try install(2, bad)
            }
        }
        try rejectedStructure("false semantic completeness") {
            var bad = structurePage("step", mode: .headings, step: 1,
                total: 1, running: false, complete: true)
            bad["semantic_complete"] = true; try install(2, bad)
        }
        for defect in ["missing", "range", "slug", "language", "kind", "name", "candidate", "evidence"] {
            try rejectedStructure("hit " + defect) {
                var bad = structurePage("step", mode: .headings, step: 1,
                    total: 1, running: false, complete: true)
                var rows = bad["hits"] as! [[String: Any]]
                var structure = rows[0]["structure"] as! [String: Any]
                switch defect {
                case "missing": rows[0].removeValue(forKey: "structure")
                case "range": structure["evidence_range"] = ["start": "1", "end": "16"]
                case "slug": structure["heading_slug"] = NSNull()
                case "language": structure["language"] = "rust"
                case "kind": structure["kind"] = "function"
                case "name": structure["name"] = String(repeating: "x", count: 4097)
                case "candidate": structure["candidate_id"] = "4097"
                default: structure["evidence_level"] = "compiler-symbol"
                }
                if defect != "missing" { rows[0]["structure"] = structure }
                bad["hits"] = rows; try install(2, bad)
            }
        }
        try rejected("structural metadata in literal query") {
            var bad = page("step", step: 1, total: 1)
            bad["requested_mode"] = "symbols"; try install(2, bad)
        }
        // One unsupported file and one partially extracted file can still
        // yield a usable result, even when metadata omissions exceed 1000.
        try setupStructure(.headings)
        try install(1, structurePage("begin-structure", mode: .headings, step: 0, catalogued: 2))
        var unsupported = structurePage("step", mode: .headings, step: 1, catalogued: 2)
        unsupported["scanned_files"] = "0"; unsupported["unavailable_files"] = "1"
        unsupported["unsupported_structure_files"] = "1"; unsupported["structure_metadata_bytes"] = "0"
        try install(2, unsupported)
        var losses = structurePage("step", mode: .headings, step: 2, total: 1,
            running: false, catalogued: 2)
        losses["scanned_files"] = "1"; losses["unavailable_files"] = "2"
        losses["unsupported_structure_files"] = "1"; losses["extraction_limited_files"] = "1"
        losses["structure_metadata_omitted"] = "1500"; losses["matches_seen"] = "1501"
        try install(2, losses, index: 1)
        var coverage: AtlasSearchReport? = try runStructure(.headings)
        check(!coverage!.complete && coverage!.hits.count == 1 && coverage!.matchesSeen == 1501
            && coverage!.unsupportedFiles == 1 && coverage!.structureCoverage?.extractionLimitedFiles == 1
            && coverage!.structureCoverage?.metadataOmitted == 1500,
            "Metadata omissions preserve the retained result rather than rejecting a large candidate count")
        check(coverage!.summary.contains("unsupported languages")
            && coverage!.summary.contains("extraction limits")
            && coverage!.summary.contains("omitted by metadata limits"),
            "Partial summary exposes each declared coverage loss")
        coverage = nil; retired()
        for defect in ["omissions", "bytes", "unsupported"] {
            try setupStructure(.headings)
            try install(1, structurePage("begin-structure", mode: .headings, step: 0, catalogued: 2))
            var first = structurePage("step", mode: .headings, step: 1, total: 1, catalogued: 2)
            first["unavailable_files"] = "1"; first["structure_metadata_omitted"] = "2"
            first["matches_seen"] = "3"; first["structure_metadata_bytes"] = "128"
            var last = structurePage("step", mode: .headings, step: 2, total: 1,
                running: false, catalogued: 2)
            last["unavailable_files"] = "1"; last["structure_metadata_omitted"] = "2"
            last["matches_seen"] = "3"; last["structure_metadata_bytes"] = "128"
            if defect == "omissions" { last["structure_metadata_omitted"] = "1" }
            if defect == "bytes" { last["structure_metadata_bytes"] = "64" }
            if defect == "unsupported" {
                first = unsupported
                last["scanned_files"] = "1"; last["unsupported_structure_files"] = "0"
            }
            try install(2, first); try install(2, last, index: 1)
            do { _ = try runStructure(.headings); preconditionFailure("Coverage regression accepted: " + defect) }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            retired()
        }
        try setupStructure(.symbols); cancelStep()
        do { _ = try runStructure(.symbols); preconditionFailure("Structural cancellation ignored") }
        catch AtlasSearchError.canceled { checks += 1 }
        retired()
        check(fixtureCount(6) == fixtureCount(7), "Racing structural cancellation releases every allocation")
        resetFixture()
        print("AtlasNativeSearchTests: \(checks) checks passed")
    }
}
