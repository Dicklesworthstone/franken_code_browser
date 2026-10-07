import Foundation

// Actual project-opening state over the production metadata decoder. These
// tests do not substitute for filesystem discovery or SwiftUI presentation.
@main private enum AtlasProjectOpeningTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ label: String) {
        precondition(value(), label); checks += 1
    }
    private static func catalog(paths: [String] = ["612e7273"], complete: Bool = true) throws -> AtlasProjectCatalog {
        let entries: [[String: Any]] = paths.enumerated().map { i, path in
            ["file_id": String(i + 1), "observed_bytes": "64", "path": ["encoding": "unix-bytes", "hex": path, "display": "escaped"],
             "x": 0, "y": 0, "w": 10, "h": 10]
        }
        let wire: [String: Any] = ["schema": "fcb.project-catalog/1", "status": "ok", "command": "catalog",
            "identity_scope": "response-local", "native_presented": false, "source_payload_read": false,
            "payload_bytes_read": "0", "read_calls": "0", "discovery_complete": complete,
            "catalogued_files": String(entries.count), "policy": "fixture", "world": ["w": 4096, "h": 4096], "files": entries]
        return try AtlasProjectCatalog.decode(JSONSerialization.data(withJSONObject: wire))
    }
    private static func context(project: UUID, atlas: UUID = UUID(), root: String = "/repo", query: String = "needle",
                                scope: String = "All files", extensions: String = "") -> AtlasSearchContext {
        .init(root: root, query: query, loadGeneration: project, atlasRevision: atlas, scope: scope, customExtensions: extensions)
    }
    @MainActor static func main() throws {
        let metadata = try catalog()
        do {
            var opening = AtlasProjectOpening()
            check(!opening.canBrowse && !opening.canPrepare && !opening.isDiscovering, "Construction is inert")
            check(opening.beginPreviews() == nil, "No source preview without a known catalog")
            let first = opening.begin()
            check(opening.isDiscovering && !opening.canBrowse && opening.accepts(first), "Metadata discovery has its own ticket")
            check(!opening.finishPreviews(first, available: true), "A discovery ticket cannot publish a text atlas")
            check(opening.acceptCatalog(metadata, for: first), "Known metadata publishes without any source/preview work")
            check(opening.canBrowse && opening.canPrepare && !opening.isDiscovering && !opening.isPreparing,
                  "File opening and search become available at metadata completion")
            check(opening.file(1, in: first.project)?.sourcePath == "a.rs", "Catalog file opens before previews")
            check(!opening.hasAtlas, "Metadata cannot claim readable text-atlas pixels")
            check(!opening.acceptCatalog(metadata, for: first), "Duplicate metadata completion cannot replace current membership")
            check(!opening.fail(first), "Late discovery failure cannot remove accepted catalog")
            let preview = opening.beginPreviews()!
            check(preview.project == first.project && preview != first, "Preview attempt differs without replacing source project")
            check(opening.isPreparing && opening.canBrowse && !opening.canPrepare, "Preview work does not lock catalog browsing")
            check(opening.file(1, in: first.project)?.sourcePath == "a.rs", "Same file remains selectable during preview work")
            check(opening.beginPreviews() == nil, "Only one preview attempt admitted, not a queue")
            check(!opening.acceptCatalog(metadata, for: preview), "Preview ticket cannot substitute project membership")
            check(opening.finishPreviews(preview, available: true), "Current preview can publish completed atlas")
            check(opening.hasAtlas && opening.canBrowse && !opening.isPreparing && !opening.canPrepare,
                  "Prepared atlas is reusable without another uncontrolled rebuild")
            check(opening.projectID == first.project && opening.file(1, in: first.project) != nil,
                  "Optional preview publication preserves independent reader origin")
            check(!opening.finishPreviews(preview, available: false), "Late duplicate result cannot erase a published atlas")
        }
        do {
            var opening = AtlasProjectOpening()
            let discovery = opening.begin()
            opening.cancel()
            check(!opening.acceptCatalog(metadata, for: discovery) && !opening.canBrowse, "Canceled discovery cannot become a late file list")
            let next = opening.begin()
            check(next.project != discovery.project, "Retry discovery creates new response-local ID domain")
            check(opening.acceptCatalog(metadata, for: next), "A new project attempt succeeds after cancellation")
            let firstPreview = opening.beginPreviews()!
            opening.cancel()
            check(opening.canBrowse && opening.canPrepare && opening.projectID == next.project, "Stop previews retains known project")
            check(opening.file(1, in: next.project)?.sourcePath == "a.rs", "Stop previews retains file authority")
            let secondPreview = opening.beginPreviews()!
            check(secondPreview.project == firstPreview.project && secondPreview != firstPreview, "Retry does not alias canceled preview")
            check(!opening.finishPreviews(firstPreview, available: true) && opening.accepts(secondPreview),
                  "Old completed preview cannot publish over active retry")
            check(!opening.fail(firstPreview) && opening.isPreparing, "Old failure cannot cancel active retry")
            check(opening.fail(secondPreview) && opening.canBrowse && opening.canPrepare, "Preview failure leaves a usable file browser")
            let thirdPreview = opening.beginPreviews()!
            check(opening.finishPreviews(thirdPreview, available: false) && opening.canPrepare, "Unavailable preview permits retry rather than empty project")
        }
        do {
            var opening = AtlasProjectOpening()
            let old = opening.begin(); opening.acceptCatalog(metadata, for: old)
            let pending = opening.beginPreviews()!
            let replacement = opening.begin()
            check(!opening.canBrowse && opening.isDiscovering && opening.projectID != old.project, "Project replacement retires membership atomically")
            check(opening.file(1, in: old.project) == nil, "Old file ID cannot authorize the replacement project")
            check(!opening.finishPreviews(pending, available: true) && opening.accepts(replacement), "Delayed old layout cannot install into new root")
            let changed = try catalog(paths: ["622e7273"])
            check(opening.acceptCatalog(changed, for: replacement), "Replacement catalog is independently accepted")
            check(opening.file(1, in: replacement.project)?.sourcePath == "b.rs" && opening.file(1, in: old.project) == nil,
                  "Same integer row from a different project is never substituted")
            opening.close()
            check(!opening.canBrowse && !opening.canPrepare && !opening.hasAtlas, "Closing window withdraws metadata and preview authority")
            check(opening.file(1, in: replacement.project) == nil && !opening.acceptCatalog(metadata, for: replacement), "Closed window receives no late row")
        }
        do {
            var opening = AtlasProjectOpening()
            let ticket = opening.begin()
            let partial = try catalog(paths: ["612e7273", "ff2e7273"], complete: false)
            opening.acceptCatalog(partial, for: ticket)
            check(opening.canBrowse && opening.canPrepare && opening.catalog?.discoveryComplete == false, "Partial membership still opens known usable files")
            check(opening.file(2, in: ticket.project)?.sourcePath == nil && opening.catalog?.entries.count == 2,
                  "Non-UTF-8 row is not dropped or given a fabricated opener")
            check(opening.file(3, in: ticket.project) == nil, "Missing row is never approximated by nearest known file")
            for paths: [String] in [[], ["ff2e7273"]] {
                let next = opening.begin()
                opening.acceptCatalog(try catalog(paths: paths), for: next)
                check(opening.canBrowse && !opening.canPrepare && opening.beginPreviews() == nil,
                      "Empty or entirely unsupported catalog is informative without useless preview work")
            }
        }
        do {
            var opening = AtlasProjectOpening()
            let ticket = opening.begin()
            opening.acceptCatalog(try catalog(paths: ["c3a92e7273", "65cc812e7273"]), for: ticket)
            check(opening.file(1, in: ticket.project)?.rawPath != opening.file(2, in: ticket.project)?.rawPath,
                  "Canonically equivalent source labels keep distinct file identities")
            var independent = AtlasProjectOpening()
            let another = independent.begin(); independent.acceptCatalog(metadata, for: another)
            opening.close()
            check(independent.canBrowse && independent.file(1, in: another.project) != nil, "Closing one window cannot revoke another's file list")
        }
        do {
            let project = UUID(), oldAtlas = UUID(), newAtlas = UUID()
            let old = context(project: project, atlas: oldAtlas), new = context(project: project, atlas: newAtlas)
            check(old.matchesSourceRequest(new), "An in-flight source search survives optional preview publication")
            check(!old.matches(new), "Old overlay/frame authority still rejected after layout change")
            check(old.matchesSourceRequest(old) && old.matches(old), "Unchanged source and display remain compatible")
            for changed in [context(project: UUID(), atlas: newAtlas), context(project: project, root: "/other"),
                            context(project: project, query: "new"), context(project: project, scope: "Rust"),
                            context(project: project, extensions: "rs")] {
                check(!old.matchesSourceRequest(changed), "Project/query/filter changes still reject late source results")
            }
            for (a, b) in [(context(project: project, root: "/répo"), context(project: project, root: "/re\u{301}po")),
                           (context(project: project, query: "é"), context(project: project, query: "e\u{301}")),
                           (context(project: project, extensions: "é"), context(project: project, extensions: "e\u{301}"))] {
                check(!a.matchesSourceRequest(b), "Source-only comparison remains raw-byte exact")
            }
        }
        do {
            var opening = AtlasProjectOpening()
            let ticket = opening.begin(); opening.acceptCatalog(metadata, for: ticket)
            var obsolete: [AtlasProjectOpening.Ticket] = []
            for _ in 0..<128 {
                let attempt = opening.beginPreviews()!
                obsolete.append(attempt); opening.cancel()
            }
            let accepted = opening.beginPreviews()!
            check(obsolete.allSatisfy { !opening.accepts($0) }, "Many retries cannot reactivate an old completion")
            check(opening.finishPreviews(accepted, available: true) && opening.projectID == ticket.project,
                  "Retries keep one catalog rather than forcing rediscovery")
        }
        print("AtlasProjectOpeningTests: \(checks) checks passed")
    }
}
