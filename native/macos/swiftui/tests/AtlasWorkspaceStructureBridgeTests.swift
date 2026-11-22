import Foundation

@_silgen_name("structure_fixture_reset") private func reset()
@_silgen_name("structure_fixture_mode") private func responseMode(_ value: Int32)
@_silgen_name("structure_fixture_status") private func statusByte(_ value: UInt8)
@_silgen_name("structure_fixture_calls") private func calls(_ kind: UInt32) -> UInt64
@_silgen_name("structure_fixture_argument") private func argument(_ index: UInt32) -> UInt64
@_silgen_name("structure_fixture_allocations") private func allocations() -> UInt64
@_silgen_name("structure_fixture_frees") private func frees() -> UInt64
@_silgen_name("structure_fixture_field_length") private func fieldLength(_ index: UInt32) -> UInt64
@_silgen_name("structure_fixture_field_is_null") private func fieldIsNull(_ index: UInt32) -> UInt8
@_silgen_name("structure_fixture_field_byte") private func fieldByte(_ index: UInt32, _ offset: UInt64) -> UInt8

@main private enum AtlasWorkspaceStructureBridgeTests {
    static let owner: UInt64 = 0xfedcba9876543210
    static let generation: UInt64 = 0x8000000100000007
    static let revision: UInt64 = 0x9000000200000009
    static let destination: UInt64 = 0xeedcba9876543211
    static let ordinal: UInt64 = 0x0030000000000011
    static let root = "/repo-é", needle = "name e\u{301} ß 😀"
    static let reply = "{\"fixture\":\"structure-λ\"}"

    static func field(_ index: UInt32) -> [UInt8]? {
        if fieldIsNull(index) == 1 { return nil }
        return (0..<fieldLength(index)).map { fieldByte(index, $0) }
    }
    static func expectArguments(_ values: [UInt64]) {
        for (index, value) in values.enumerated() { precondition(argument(UInt32(index)) == value) }
    }
    static func expectField(_ index: UInt32, _ value: String?) {
        precondition(field(index) == value.map { Array($0.utf8) })
    }
    // Evaluate throwing bridge calls before entering precondition's
    // nonthrowing autoclosure.
    static func check(_ value: Bool) { precondition(value) }
    static func request(mode: AtlasWorkspaceStructureMode = .symbols,
        nameMode: AtlasSearchNameMode = .contains, fileScope: AtlasSearchFileScope = .all,
        pathScope: AtlasSearchPathScope = .workspace, policy: AtlasDiscoveryPolicy = .repositoryRules)
        throws -> AtlasWorkspaceStructureRequest {
        try .init(needle: needle, mode: mode, nameMode: nameMode, fileScope: fileScope,
            pathScope: pathScope, discoveryPolicy: policy, maximumMatches: 137,
            maximumScanBytes: 1 << 32, maximumReadCalls: 1_048_575, startAfter: ordinal)
    }
    static func expectInvalidInput(_ action: () throws -> Void) {
        do { try action(); preconditionFailure("Invalid native bridge input was accepted") }
        catch AtlasWorkspaceStructureError.invalidInput { }
        catch { preconditionFailure("Unexpected input refusal: \(error)") }
    }

    static func main() throws {
        reset()
        let transport = AtlasWorkspaceStructureTransport.native
        precondition(transport.create() == owner && calls(0) == 1)
        let modes: [AtlasWorkspaceStructureMode] = [.symbols, .headings]
        let names: [AtlasSearchNameMode] = [.exact, .prefix, .contains]
        let policies: [AtlasDiscoveryPolicy] = [.repositoryRules, .includeExcluded]
        for (modeIndex, mode) in modes.enumerated() {
            for (nameIndex, nameMode) in names.enumerated() {
                for (policyIndex, policy) in policies.enumerated() {
                    let requested = try request(mode: mode, nameMode: nameMode, policy: policy)
                    let result = try transport.open(owner, root, generation, requested)
                    precondition(result == reply)
                    expectArguments([owner, generation, UInt64(needle.utf8.count), UInt64(modeIndex),
                        UInt64(nameIndex), ordinal, 0, 137, 1 << 32, 1_048_575, UInt64(policyIndex)])
                    expectField(0, root); expectField(1, needle)
                    expectField(2, nil); expectField(3, nil); expectField(4, nil)
                    precondition(allocations() == frees())
                }
            }
        }

        // A non-null empty predicate must stay distinguishable from "all".
        let empty = try request(fileScope: .init(extensions: []))
        check(try transport.open(owner, root, generation, empty) == reply)
        expectField(2, ""); expectField(3, ""); expectField(4, nil)
        let types = try AtlasSearchFileScope(extensions: ["RS", "md"], filenames: ["README.md"])
        let directory = try request(mode: .headings, fileScope: types,
            pathScope: .init(kind: .directory, path: "src/café"))
        check(try transport.open(owner, root, generation, directory) == reply)
        precondition(argument(6) == 1)
        expectField(2, "md,rs"); expectField(3, "readme.md"); expectField(4, "src/café")
        let file = try request(pathScope: .init(kind: .file, path: "src/e\u{301}.rs"))
        check(try transport.open(owner, root, generation, file) == reply)
        precondition(argument(6) == 2); expectField(4, "src/e\u{301}.rs")
        #if canImport(CryptoKit)
        let visible = try request(pathScope: .init(visiblePaths: [
            [0xff, 0x2e, 0x6d, 0x64], Array("a.rs".utf8), Array("a.rs".utf8)
        ]), policy: .includeExcluded)
        check(try transport.open(owner, root, generation, visible) == reply)
        precondition(argument(6) == 3 && argument(10) == 1)
        expectField(4, "612e7273,ff2e6d64")
        let noVisibleFiles = try request(pathScope: .init(visiblePaths: []))
        check(try transport.open(owner, root, generation, noVisibleFiles) == reply)
        precondition(argument(6) == 3); expectField(4, "")
        #endif

        // Each position is deliberately beyond IEEE-754's exact integer range;
        // opaque bridge arguments must keep all UInt64 bits and their order.
        check(try transport.advance(owner, generation, revision, 16) == reply)
        expectArguments([owner, generation, revision, 16])
        check(try transport.page(owner, generation, revision, ordinal, 32) == reply)
        expectArguments([owner, generation, revision, ordinal, 32])
        check(try transport.stop(owner, generation) == reply)
        expectArguments([owner, generation])
        check(try transport.openReader(owner, destination, generation, revision, ordinal) == reply)
        expectArguments([owner, destination, generation, revision, ordinal])
        for value in [UInt8(0), 1, 2, 255] {
            statusByte(value)
            precondition(transport.cancel(owner) == (value == 1)); expectArguments([owner])
            precondition(transport.close(owner) == (value == 1)); expectArguments([owner])
        }
        statusByte(1)

        let requested = try request()
        let before = calls(1)
        for invalidRoot in ["", "relative", "/bad\0root", "/" + String(repeating: "x", count: 16_384)] {
            expectInvalidInput { _ = try transport.open(owner, invalidRoot, generation, requested) }
        }
        expectInvalidInput { _ = try transport.open(0, root, generation, requested) }
        expectInvalidInput { _ = try transport.open(owner, root, 0, requested) }
        precondition(calls(1) == before, "Rejected input reached the C boundary")

        let commands: [() throws -> String?] = [
            { try transport.open(owner, root, generation, requested) },
            { try transport.advance(owner, generation, revision, 4) },
            { try transport.page(owner, generation, revision, 0, 32) },
            { try transport.stop(owner, generation) },
            { try transport.openReader(owner, destination, generation, revision, 0) },
        ]
        for command in commands {
            responseMode(1)
            let allocatedBefore = allocations()
            check(try command() == nil)
            precondition(allocations() == allocatedBefore && allocations() == frees())
            for mode in [Int32(2), 3, 4, 5] {
                responseMode(mode)
                let released = frees()
                do { _ = try command(); preconditionFailure("Empty, malformed, oversized or unterminated C reply accepted") }
                catch AtlasWorkspaceStructureError.invalidResponse { }
                precondition(frees() == released + 1 && allocations() == frees())
            }
        }
        responseMode(6)
        let boundary = try transport.page(owner, generation, revision, 0, 1)
        precondition(boundary?.utf8.count == AtlasWorkspaceStructureLimits.responseBytes)
        precondition(allocations() == frees())
        responseMode(0)
        let retainedCopy = try transport.page(owner, generation, revision, 0, 1)
        _ = try transport.advance(owner, generation, revision, 4)
        precondition(retainedCopy == reply, "Foreign allocation escaped instead of becoming an owned Swift copy")
        precondition(allocations() == frees())
        reset()
        print("Structural search ABI argument, Unicode scope, full-width identity and owned-buffer fixtures passed.")
    }
}
