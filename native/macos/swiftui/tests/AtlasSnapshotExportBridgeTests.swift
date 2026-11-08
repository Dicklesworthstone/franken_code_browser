import Foundation
@_silgen_name("snapshot_export_fixture_reset") private func reset()
@_silgen_name("snapshot_export_fixture_mode") private func mode(_ value: Int32)
@_silgen_name("snapshot_export_fixture_calls") private func calls() -> UInt32
@_silgen_name("snapshot_export_fixture_live") private func live() -> UInt32
@_cdecl("snapshot_export_fixture_cancel") func cancelSnapshotExportFixture(_ context: UnsafeMutableRawPointer?) {
    guard let context else { preconditionFailure("Missing live callback context") }
    Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().cancel()
}

@main private enum AtlasSnapshotExportBridgeTests {
    static func main() throws {
        reset()
        let transport = AtlasSnapshotExportTransport.native
        let request = try AtlasSnapshotExportRequest(root: "/repo", destination: "/out/project.fcbs")
        let first = try transport.save(request, AtlasSearchCancellation())
        precondition(first == "{\"owned\":true}")
        precondition(calls() == 1 && live() == 0)
        mode(1)
        let absent = try transport.save(request, AtlasSearchCancellation())
        precondition(absent == nil && live() == 0)
        for bad: Int32 in [2, 3] {
            mode(bad)
            do { _ = try transport.save(request, AtlasSearchCancellation()); preconditionFailure("Unverified bridge bytes admitted") }
            catch AtlasSnapshotExportError.unverifiedOutcome { }
            precondition(live() == 0)
        }
        mode(4)
        let late = AtlasSearchCancellation()
        let result = try transport.save(request, late)
        precondition(late.isCanceled && result == "{\"owned\":true}" && live() == 0,
            "Post-call cancellation must preserve the writer's returned receipt")
        let before = calls()
        do { _ = try transport.save(request, late); preconditionFailure("Pre-entry cancellation ignored") }
        catch AtlasSnapshotExportError.notStarted { }
        precondition(calls() == before)
        mode(5)
        let lost = AtlasSearchCancellation()
        let unknown = try transport.save(request, lost)
        precondition(lost.isCanceled && unknown == nil && live() == 0,
            "Null after entered cancellation remains an unknown outcome")
        mode(7)
        let metadataOnly = try AtlasSnapshotExportRequest(root: "/repo", destination: "/out/project.fcbs",
            limits: AtlasSnapshotExportLimits(files: 1, fileBytes: 0, sourceBytes: 0, respectIgnores: false))
        let bounded = try transport.save(metadataOnly, AtlasSearchCancellation())
        precondition(bounded != nil && live() == 0)
        print("Snapshot export bridge ABI, cancellation and owned-allocation fixtures passed.")
    }
}
