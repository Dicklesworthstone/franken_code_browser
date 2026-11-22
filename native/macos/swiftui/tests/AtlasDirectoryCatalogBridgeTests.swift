import Foundation
@_silgen_name("project_fixture_set") private func configureFixture(_ text: UnsafePointer<CChar>?, _ cancel: Int32)
@_silgen_name("project_fixture_count") private func count(_ kind: UInt32) -> UInt32
@_silgen_name("project_fixture_policy") private func requestedPolicy() -> UInt64
@_silgen_name("project_fixture_directory_length") private func directoryLength() -> UInt64
@_silgen_name("project_fixture_directory_byte") private func directoryByte(_ index: UInt64) -> UInt8
@_cdecl("project_fixture_cancel") func directoryFixtureCancel(_ context: UnsafeMutableRawPointer?) {
    guard let context else { preconditionFailure("Missing cancellation context") }
    Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().cancel()
}
@main private enum AtlasDirectoryCatalogBridgeTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message); checks += 1
    }
    private static func configure(_ wire: [String: Any], cancel: Int32 = 0) throws {
        let text = String(decoding: try JSONSerialization.data(withJSONObject: wire), as: UTF8.self)
        text.withCString { configureFixture($0, cancel) }
    }
    @MainActor static func main() throws {
        let directory = try AtlasDirectoryFocus(rawPath: [255, 47, 115])
        let valid: [String: Any] = [
            "schema": "fcb.project-directory-catalog/1", "command": "directory-catalog", "status": "ok",
            "identity_scope": "response-local", "native_presented": false, "source_payload_read": false,
            "payload_bytes_read": "0", "read_calls": "0", "catalogued_files": "1", "discovery_complete": false,
            "policy": "repository-rules-v1", "path_basis": "project-root", "world": ["w": 4096, "h": 4096],
            "directory": ["encoding": "unix-bytes", "hex": "ff2f73", "display": "escaped directory"],
            "files": [["file_id": "1", "observed_bytes": "5", "x": 0, "y": 0, "w": 1, "h": 1,
                       "path": ["encoding": "unix-bytes", "hex": "ff2f732f612e7273", "display": "escaped file"]]]]
        try configure(valid)
        let report = try AtlasProjectWorker.catalogReport(root: "/project", directory: directory,
            cancellation: AtlasSearchCancellation())
        check(directoryLength() == 3 && (0..<3).map { directoryByte(UInt64($0)) } == directory.rawPath,
            "Raw directory bytes and explicit length cross C without UTF8 replacement")
        check(report.requestedDirectory == directory && report.requestedRoot == "/project"
            && !report.discoveryComplete && report.entries[0].rawPath == [255, 47, 115, 47, 97, 46, 114, 115],
            "Project-relative identity and partial coverage survive transport")
        check(requestedPolicy() == 0 && count(0) == 1 && count(1) == 1 && count(2) == 1,
            "Default policy and allocation ownership")
        var included = valid; included["policy"] = "all-regular-files/no-rule-files-v1"
        try configure(included)
        let all = try AtlasProjectWorker.catalogReport(root: "/project", directory: directory, discoveryPolicy: .includeExcluded,
            cancellation: AtlasSearchCancellation())
        check(requestedPolicy() == 1 && all.matchesDiscoveryPolicy(AtlasDiscoveryPolicy.includeExcluded.wireName)
            && count(1) == count(2), "Directory policy reaches C and binds receipt")
        var root = valid; root["schema"] = "fcb.project-catalog/1"; root["command"] = "catalog"
        root.removeValue(forKey: "directory"); root.removeValue(forKey: "path_basis")
        try configure(root)
        let rootReport = try AtlasProjectWorker.catalogReport(root: "/project", directory: .project,
            cancellation: AtlasSearchCancellation())
        check(directoryLength() == 0 && rootReport.requestedDirectory == .project, "Root dispatch preserves established endpoint")
        for bad in [root, included, valid.merging(["path_basis": "directory"]) { _, new in new },
                    valid.merging(["directory": ["encoding": "unix-bytes", "hex": "737263", "display": "spoofed"]]) { _, new in new }] {
            try configure(bad)
            do {
                _ = try AtlasProjectWorker.catalogReport(root: "/project", directory: directory,
                    cancellation: AtlasSearchCancellation())
                preconditionFailure("Wrong directory receipt admitted")
            } catch AtlasProjectCatalog.DecodeError.invalidResponse {
                check(count(1) == 1 && count(2) == 1, "Wrong schema, policy, basis or raw scope releases reply")
            }
        }
        let canceled = AtlasSearchCancellation(); canceled.cancel()
        try configure(valid)
        do {
            _ = try AtlasProjectWorker.catalogReport(root: "/project", directory: directory, cancellation: canceled)
            preconditionFailure("Pre-canceled directory work entered C")
        } catch AtlasProjectIOError.canceled { check(count(0) == 0, "Pre-cancel prevents directory work") }
        try configure(valid, cancel: 1)
        do {
            _ = try AtlasProjectWorker.catalogReport(root: "/project", directory: directory, cancellation: AtlasSearchCancellation())
            preconditionFailure("Canceled response published")
        } catch AtlasProjectIOError.canceled { check(count(1) == 1 && count(2) == 1, "Late cancellation frees returned allocation") }
        configureFixture(nil, 0)
        do {
            _ = try AtlasProjectWorker.catalogReport(root: "/project", directory: directory, cancellation: AtlasSearchCancellation())
            preconditionFailure("Null treated as empty directory")
        } catch AtlasProjectIOError.unavailable { check(count(1) == 0 && count(2) == 0, "Unavailable does not establish absence") }
        String(repeating: "x", count: AtlasProjectCatalog.maximumResponseBytes + 1).withCString { configureFixture($0, 0) }
        do {
            _ = try AtlasProjectWorker.catalogReport(root: "/project", directory: directory, cancellation: AtlasSearchCancellation())
            preconditionFailure("Oversized directory payload admitted")
        } catch AtlasProjectIOError.limit { check(count(1) == 1 && count(2) == 1, "Oversized reply bounded before decoding and freed") }
        configureFixture(nil, 0)
        print("AtlasDirectoryCatalogBridgeTests: \(checks) checks passed")
    }
}
