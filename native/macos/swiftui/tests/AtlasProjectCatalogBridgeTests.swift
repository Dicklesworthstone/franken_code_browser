import Foundation
@_silgen_name("project_fixture_set") private func configure(_ text: UnsafePointer<CChar>?, _ cancel: Int32)
@_silgen_name("project_fixture_count") private func count(_ kind: UInt32) -> UInt32
@_cdecl("project_fixture_cancel") func projectFixtureCancel(_ context: UnsafeMutableRawPointer?) {
    guard let context else { preconditionFailure("Missing cancellation context") }
    Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().cancel()
}
@main private enum AtlasProjectCatalogBridgeTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message); checks += 1
    }
    @MainActor static func main() throws {
        let json = #"{"schema":"fcb.project-catalog/1","status":"ok","command":"catalog","identity_scope":"response-local","native_presented":false,"source_payload_read":false,"payload_bytes_read":"0","read_calls":"0","discovery_complete":false,"catalogued_files":"1","policy":"fixture","world":{"w":4096,"h":4096},"files":[{"file_id":"9007199254740993","path":{"encoding":"unix-bytes","hex":"ff2e7273","display":"escaped raw name"},"observed_bytes":"5","x":0,"y":0,"w":1,"h":1}]}"#
        json.withCString { configure($0, 0) }
        let report = try AtlasProjectWorker.catalogReport(root: "/project", cancellation: AtlasSearchCancellation())
        check(!report.discoveryComplete && report.entries.count == 1, "Partial metadata crosses production native adapter")
        check(report.entries[0].sourcePath == nil && report.entries[0].id == 9_007_199_254_740_993, "Raw name and full-width ID preserved")
        check(count(0) == 1 && count(1) == 1 && count(2) == 1, "One metadata call and one owned response release")
        for invalid in ["", "/a\0b", String(repeating: "a", count: 16_385)] {
            json.withCString { configure($0, 0) }
            do { _ = try AtlasProjectWorker.catalogReport(root: invalid, cancellation: AtlasSearchCancellation()); preconditionFailure("Invalid input") }
            catch AtlasProjectIOError.invalidRequest { check(count(0) == 0, "Invalid input never reaches C") }
        }
        let flag = AtlasSearchCancellation(); flag.cancel()
        json.withCString { configure($0, 0) }
        do { _ = try AtlasProjectWorker.catalogReport(root: "/project", cancellation: flag); preconditionFailure("Pre-cancel ignored") }
        catch AtlasProjectIOError.canceled { check(count(0) == 0, "Pre-cancel prevents metadata work") }
        json.withCString { configure($0, 1) }
        do { _ = try AtlasProjectWorker.catalogReport(root: "/project", cancellation: AtlasSearchCancellation()); preconditionFailure("Late cancel ignored") }
        catch AtlasProjectIOError.canceled { check(count(1) == 1 && count(2) == 1, "Cancellation racing reply still releases its allocation") }
        configure(nil, 0)
        do { _ = try AtlasProjectWorker.catalogReport(root: "/project", cancellation: AtlasSearchCancellation()); preconditionFailure("Null response") }
        catch AtlasProjectIOError.unavailable { check(count(1) == 0 && count(2) == 0, "Null is refusal, not empty metadata") }
        for bad in ["{}", "not json", json.replacingOccurrences(of: "\"source_payload_read\":false", with: "\"source_payload_read\":true")] {
            bad.withCString { configure($0, 0) }
            do { _ = try AtlasProjectWorker.catalogReport(root: "/project", cancellation: AtlasSearchCancellation()); preconditionFailure("Bad response") }
            catch { check(count(1) == count(2), "Bad response releases C allocation") }
        }
        String(repeating: "x", count: AtlasProjectCatalog.maximumResponseBytes + 1).withCString { configure($0, 0) }
        do { _ = try AtlasProjectWorker.catalogReport(root: "/project", cancellation: AtlasSearchCancellation()); preconditionFailure("Oversized response") }
        catch AtlasProjectIOError.limit { check(count(1) == count(2), "Oversized reply bounded before Data decoding and released") }
        configure(nil, 0)
        print("AtlasProjectCatalogBridgeTests: \(checks) checks passed")
    }
}
