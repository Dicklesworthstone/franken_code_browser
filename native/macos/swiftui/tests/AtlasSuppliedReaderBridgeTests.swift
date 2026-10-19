import Foundation

@_silgen_name("supplied_fixture_reset") private func resetSuppliedFixture()
@_silgen_name("supplied_fixture_mode") private func suppliedMode(_ value: Int32)
@_silgen_name("supplied_fixture_calls") private func suppliedCalls() -> UInt32
@_silgen_name("supplied_fixture_live") private func suppliedLive() -> UInt32
// The bundle provides this shared declaration from App.swift.
@_silgen_name("fcb_free_string") func fcb_free_string(_ pointer: UnsafeMutablePointer<CChar>?)

/// Real CryptoKit SHA and production C marshaling with a checked pointer fixture.
/// Invoke on macOS through strict RCH; no Rust or physical-UI claim is implied.
@main @MainActor private enum AtlasSuppliedReaderBridgeTests {
    static func main() async {
        resetSuppliedFixture()
        let handle = AtlasReaderTransport.native.create()
        precondition(handle == 18_446_744_073_709_551_500)
        let text = "\u{feff}abc\0e\u{301}\r\n", path = "/project/e\u{301}.md"
        let bytes = Data(text.utf8)
        let response = await Task.detached {
            AtlasReaderTransport.native.openBytes?(handle, path, bytes)
        }.value
        precondition(response?.json == "{\"fixture\":true}")
        precondition(response?.sha256 == "6a54fa7ae81fb568919d1e4b5e3c5a987679c865cb1eae6da14f2258f3f0bafc")
        precondition(suppliedCalls() == 1 && suppliedLive() == 0)

        suppliedMode(1)
        let empty = await Task.detached {
            AtlasReaderTransport.native.openBytes?(handle, path, Data())
        }.value
        precondition(empty?.json != nil && empty?.sha256 == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        precondition(suppliedCalls() == 2 && suppliedLive() == 0)

        suppliedMode(2)
        let refused = await Task.detached {
            AtlasReaderTransport.native.openBytes?(handle, path, bytes)
        }.value
        precondition(refused?.json == nil && suppliedCalls() == 3 && suppliedLive() == 0)

        for label in ["", "a\0b", String(repeating: "p", count: 16_385)] {
            let invalid = await Task.detached {
                AtlasReaderTransport.native.openBytes?(handle, label, bytes)
            }.value
            precondition(invalid == nil, "invalid label crossed the native boundary")
        }
        let over = Data(repeating: 0, count: Int(AtlasReaderLimits.captureBytes) + 1)
        let oversized = await Task.detached {
            AtlasReaderTransport.native.openBytes?(handle, path, over)
        }.value
        precondition(oversized == nil && suppliedCalls() == 3 && suppliedLive() == 0)
        // The production bridge must preserve all five UInt64 arguments and
        // release the native response, including generation values above Int64.
        guard let comparison = AtlasReaderTransport.native.comparison else {
            fatalError("native retained comparison capability missing")
        }
        precondition(comparison.domain == AtlasReaderTransport.native.comparison?.domain)
        let compared = await Task.detached {
            comparison.compare(handle, handle - 1, UInt64.max, 512, 64 * 1024 * 1024)
        }.value
        precondition(compared == "{\"comparison_fixture\":true}")
        precondition(suppliedCalls() == 4 && suppliedLive() == 0)
        precondition(AtlasReaderTransport.native.close(handle))
        print("AtlasSuppliedReaderBridgeTests: exact raw buffer, SHA, empty capture, limits, pair arguments and allocation checks passed")
    }
}
