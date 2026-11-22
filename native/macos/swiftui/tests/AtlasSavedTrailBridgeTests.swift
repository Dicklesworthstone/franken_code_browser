import Foundation

@_silgen_name("fixture_saved_trail_mode") private func mode(_ value: Int32)
@_silgen_name("fixture_saved_trail_count") private func count(_ index: UInt64) -> UInt64

@main private enum AtlasSavedTrailBridgeTests {
    static func main() throws {
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
        let fixture = SavedRepositoryFixture(searchVectors: true)
        let identity = try JSONDecoder().decode(AtlasSavedIdentity.self, from: Data(fixture.encode(fixture.header(owner, "info")).utf8))
        let member = try JSONDecoder().decode(AtlasSavedMember.self, from: Data(fixture.encode(fixture.memberObject(1)).utf8))
        let readerIdentity = AtlasReaderIdentity(owner: reader, file: 1, revision: 1, capturedBytes: 209,
            pathBytes: fixture.rawPath, encoding: "utf8", displayPath: member.path.display)
        let selection = try AtlasSavedTrailSelection(identity: identity, member: member,
            readerIdentity: readerIdentity, start: 202, end: 208)
        let point = try AtlasSavedTrailSelection(identity: identity, member: member,
            readerIdentity: readerIdentity, start: 209, end: 209)
        let title = "e\u{301}\0T", rationale = "why\0é\n", destination = "/write-e\u{301}.fcbt"
        let parent = AtlasSavedTrailParent(AtlasSavedTrailPage(identity: identity, generation: generation, lastAttempt: generation,
            digest: String(repeating: "a", count: 64), path: member.path, title: title, parentDigest: nil,
            total: 1, referenceBytes: 5, retainedBytes: 512, ready: 1, unavailable: 0, readCalls: 1,
            start: 0, limit: 32, items: [], next: nil))
        let new = try AtlasSavedTrailWriteRequest(selection: selection, operation: .max, parent: nil,
            title: title, rationale: rationale, destination: destination)
        let append = try AtlasSavedTrailWriteRequest(selection: point, operation: .max - 1, parent: parent,
            title: title, rationale: rationale, destination: destination)
        let empty = try AtlasSavedTrailWriteRequest(selection: selection, operation: .max - 2, parent: nil,
            title: "", rationale: "", destination: destination)
        mode(0)
        for request in [new, append, empty] { precondition(transport.write!(owner, request) == "{}") }
        precondition(transport.writeReceipt!(owner, .max) == "{}" && count(5) == 3 && count(6) == 1 && count(4) == 20)
        precondition(transport.write!(0, new) == nil && transport.write!(owner - 1, new) == nil)
        precondition(transport.writeReceipt!(0, .max) == nil && transport.writeReceipt!(owner, 0) == nil)
        precondition(count(5) == 3 && count(6) == 1)
        mode(1)
        precondition(transport.write!(owner, new) == nil && transport.writeReceipt!(owner, .max) == nil && count(4) == 20)
        for invalidMode in [Int32(2), 3, 4] {
            mode(invalidMode)
            precondition(transport.write!(owner, new) == nil && transport.writeReceipt!(owner, .max) == nil)
        }
        precondition(count(4) == 26, "every nonnull write response must be freed exactly once")
        let export = try AtlasSavedTrailExportRequest(trail: parent, operation: .max,
            destination: "/pack-e\u{301}.json", limits: AtlasSavedTrailExportLimits(selectedBytes: 4 * 1024 * 1024))
        mode(0)
        precondition(transport.export!(owner, export) == "{}" && transport.exportReceipt!(owner, .max) == "{}")
        precondition(count(7) == 1 && count(8) == 1 && count(4) == 28)
        precondition(transport.export!(0, export) == nil && transport.export!(owner - 1, export) == nil)
        precondition(transport.exportReceipt!(0, .max) == nil && transport.exportReceipt!(owner, 0) == nil)
        mode(1)
        precondition(transport.export!(owner, export) == nil && transport.exportReceipt!(owner, .max) == nil && count(4) == 28)
        for invalidMode in [Int32(2), 3, 4] {
            mode(invalidMode)
            precondition(transport.export!(owner, export) == nil && transport.exportReceipt!(owner, .max) == nil)
        }
        precondition(count(4) == 34, "every nonnull export response must be freed exactly once")
        let mixed = try AtlasSavedTrailExportRequest(trail: parent, operation: .max,
            destination: export.destination, limits: export.limits, additionalArchives: ["/extra-e\u{301}.fcbs", "/extra-é.fcbs"])
        let maximum = try AtlasSavedTrailExportRequest(trail: parent, operation: .max,
            destination: export.destination, limits: export.limits, additionalArchives: (0..<7).map { "/extra-\($0).fcbs" })
        let beforeMixed = count(9), beforeFreed = count(4), beforeLegacy = count(7)
        mode(0)
        for request in [mixed, maximum, export] {
            precondition(transport.exportWithArchives!(owner, request, AtlasSearchCancellation()) == "{}")
        }
        precondition(count(9) == beforeMixed + 3 && count(4) == beforeFreed + 3)
        precondition(transport.export!(owner, mixed) == nil && count(7) == beforeLegacy,
            "legacy export silently discarded selected archive authority")
        precondition(transport.exportWithArchives!(0, mixed, AtlasSearchCancellation()) == nil)
        precondition(transport.exportWithArchives!(owner - 1, mixed, AtlasSearchCancellation()) == nil)
        let canceled = AtlasSearchCancellation(); canceled.cancel()
        precondition(transport.exportWithArchives!(owner, mixed, canceled) == nil && count(9) == beforeMixed + 3,
            "cancellation before entry reached the foreign writer")
        mode(1)
        precondition(transport.exportWithArchives!(owner, mixed, AtlasSearchCancellation()) == nil)
        precondition(count(4) == beforeFreed + 3)
        for invalidMode in [Int32(2), 3, 4] {
            mode(invalidMode)
            precondition(transport.exportWithArchives!(owner, mixed, AtlasSearchCancellation()) == nil)
        }
        precondition(count(9) == beforeMixed + 7 && count(4) == beforeFreed + 6,
            "every nonnull mixed export response must be freed exactly once")
        print("Saved trail public-header ABI, full-width IDs, exact archive arrays, cancellation and response ownership fixtures passed")
    }
}
