import Foundation

@_silgen_name("fixture_saved_trail_mode") private func mode(_ value: Int32)
@_silgen_name("fixture_saved_trail_count") private func count(_ index: UInt64) -> UInt64

@main private enum AtlasSavedTrailBridgeTests {
    static func main() {
        let transport = AtlasSavedTrailTransport.native
        let owner = UInt64.max - 10, generation = UInt64.max - 9, reader = UInt64.max - 8
        let path = "/trail-e\u{301}.fcbt"
        let requests: [AtlasSavedTrailRequest] = [
            .attach(generation: generation, path: path), .page(generation: generation, start: 1000, limit: 24),
            .detach(generation: generation), .openReader(reader: reader, generation: generation, item: 1023)]
        for request in requests { precondition(transport.run(owner, request) == "{}") }
        precondition((0..<4).allSatisfy { count(UInt64($0)) == 1 } && count(4) == 4)
        for path in ["relative.fcbt", "/nul\0.fcbt", "/" + String(repeating: "x", count: 16_384)] {
            precondition(transport.run(owner, .attach(generation: generation, path: path)) == nil)
        }
        let invalid: [AtlasSavedTrailRequest] = [
            .attach(generation: 0, path: path), .page(generation: 0, start: 1000, limit: 24),
            .page(generation: generation, start: 1025, limit: 1), .page(generation: generation, start: 0, limit: 33),
            .page(generation: generation, start: 0, limit: 0), .detach(generation: 0),
            .openReader(reader: owner, generation: generation, item: 0),
            .openReader(reader: 0, generation: generation, item: 0),
            .openReader(reader: reader, generation: 0, item: 0),
            .openReader(reader: reader, generation: generation, item: 1024)]
        for request in invalid { precondition(transport.run(owner, request) == nil) }
        for request in requests { precondition(transport.run(0, request) == nil) }
        precondition((0..<4).allSatisfy { count(UInt64($0)) == 1 } && count(4) == 4,
            "invalid input crossed the foreign boundary")
        mode(1)
        for request in requests { precondition(transport.run(owner, request) == nil) }
        precondition(count(4) == 4, "null response must not be freed")
        for invalidMode in [Int32(2), 3, 4] {
            mode(invalidMode)
            for request in requests { precondition(transport.run(owner, request) == nil) }
        }
        precondition(count(4) == 16, "all nonnull rejected responses must be freed exactly once")
        print("Saved trail public-header ABI, full-width IDs, exact paths and response ownership fixtures passed")
    }
}
