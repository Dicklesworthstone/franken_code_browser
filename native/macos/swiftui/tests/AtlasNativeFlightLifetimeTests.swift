import AppKit
import CoreText
import Metal

/// Real command completion, independent of a live presenter or a fake timer.
/// A shared-event gate holds the GPU until all presentation-side owners drop.
@main struct AtlasNativeFlightLifetimeTests {
    @MainActor static func main() {
        _ = NSApplication.shared
        guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue(),
              let gate = device.makeSharedEvent() else { preconditionFailure("Metal command fixture unavailable") }
        let pool = AtlasNativeRetirement(maximumSlots: 1, maximumBytes: 4096)
        var lease: AtlasNativeRetirement.Lease? = pool.reserve(bytes: 4096)
        precondition(lease?.publish(storage: Data(repeating: 7, count: 4096), bytes: 4096) == true)
        weak var observedLease = lease
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: "source lifetime"))
        var first: AtlasTextTile? = AtlasTextTile(path: "first.rs", lines: [line],
            sourceRange: NSRange(location: 0, length: 15), part: 0)
        var continuation: AtlasTextTile? = AtlasTextTile(path: "first.rs", lines: [line],
            sourceRange: NSRange(location: 15, length: 15), part: 1)
        first?.nativeResidencyOwner = lease; continuation?.nativeResidencyOwner = lease
        var command = queue.makeCommandBuffer()
        guard command != nil else { preconditionFailure("Metal command allocation") }
        command!.encodeWaitForEvent(gate, value: 1)
        precondition(AtlasMetalPresentation.retainSourceLifetimes(of: [first!, continuation!], through: command!) == 1,
                     "a file's continuation tiles share one command-owned lifetime")
        command!.commit()
        // Dropping the presenter would drop its frame arrays in the same way.
        // The command's completion closure is the sole remaining lease owner.
        first = nil; continuation = nil; lease = nil
        for _ in 0..<4 { _ = RunLoop.main.run(mode: .default, before: Date(timeIntervalSinceNow: 0.001)) }
        precondition(observedLease != nil && pool.reservedSlots == 1 && pool.reservedBytes == 4096,
                     "a pending GPU command must retain source teardown credit after its presenter closes")
        precondition(pool.reserve(bytes: 1) == nil, "in-flight credit cannot be reused")
        gate.signaledValue = 1
        let deadline = Date(timeIntervalSinceNow: 10)
        while command!.status != .completed && command!.status != .error && Date() < deadline {
            _ = RunLoop.main.run(mode: .default, before: Date(timeIntervalSinceNow: 0.001))
        }
        precondition(command!.status == .completed, "real GPU command must reach terminal completion")
        command = nil
        while pool.reservedSlots != 0 && Date() < deadline {
            _ = RunLoop.main.run(mode: .default, before: Date(timeIntervalSinceNow: 0.001))
        }
        precondition(observedLease == nil && pool.reservedSlots == 0 && pool.reservedBytes == 0,
                     "terminal completion and off-main storage drain return the reserved credit")
        print("Native source flight lifetime: deduplicated command ownership survives presenter teardown until GPU completion")
    }
}
