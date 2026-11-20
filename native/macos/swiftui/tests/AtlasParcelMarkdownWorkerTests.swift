import Foundation

/// Wire fixtures exercise the actual supplied-capture worker, document jobs and
/// decoders. They do not parse Markdown or impersonate a native display font.
private final class ParcelFixture: @unchecked Sendable {
    enum Fault: CaseIterable {
        case none, digest, expectedDigest, bytes, path, origin, initialRead, initialCalls
        case inheritedQuery, inheritedOutline, inheritedDocument, missingGeneration
        case flowIncomplete, flowGeneration, unqualifiedDisplay, displayWidth, displayLimit
        case missingPage, repeatedPage, incompletePage, stalePage, changedInventory, missingFont, malformedFont
    }
    struct State {
        var next: UInt64 = 1
        var calls: [String] = []
        var closed: [UInt64: Int] = [:]
        var supplied: [Data] = []
        var canceled = false
        var cancelAt: String?
        var closeAllowed = true
        var fault = Fault.none
    }
    let vector: AtlasSuppliedReaderVector
    let label = "docs/e\u{301}.md"
    private let lock = NSLock()
    private var state = State()
    init(_ vector: AtlasSuppliedReaderVector = .abc) { self.vector = vector }
    func change<Value>(_ body: (inout State) -> Value) -> Value {
        lock.lock(); defer { lock.unlock() }; return body(&state)
    }
    var snapshot: State { change { $0 } }
    private func record(_ stage: String) -> State {
        change {
            $0.calls.append(stage)
            if $0.cancelAt == stage { $0.canceled = true }
            return $0
        }
    }
    private func json(_ object: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: object), as: UTF8.self)
    }
    private func path(_ value: String) -> [String: String] {
        ["encoding": "unix-bytes", "hex": value.utf8.map { String(format: "%02x", $0) }.joined(), "display": value]
    }
    private func header(_ command: String, handle: UInt64) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
         "owner": String(handle), "file_id": "1", "source_revision": "1",
         "captured_bytes": String(vector.text.utf8.count), "encoding": "utf8",
         "additional_source_bytes_read": "0", "native_presented": false, "path": path(label)]
    }
    private func sourceInfo(_ handle: UInt64, fault: Fault) -> String {
        var w = header("info", handle: handle)
        w["capture_origin"] = "host-supplied"; w["capture_sha256"] = vector.sha256
        w["initial_source_bytes_read"] = "0"; w["initial_read_calls"] = "0"
        w["accepted_query_generation"] = NSNull(); w["accepted_outline_generation"] = NSNull()
        w["accepted_document_generation"] = NSNull(); w["accepted_reference_generation"] = NSNull()
        switch fault {
        case .digest: w["capture_sha256"] = String(repeating: "0", count: 64)
        case .bytes: w["captured_bytes"] = String(vector.text.utf8.count + 1)
        case .path: w["path"] = path("docs/é.md")
        case .origin: w["capture_origin"] = "regular-file-observation-not-atomic"
        case .initialRead: w["initial_source_bytes_read"] = "1"
        case .initialCalls: w["initial_read_calls"] = "1"
        case .inheritedQuery: w["accepted_query_generation"] = "1"
        case .inheritedOutline: w["accepted_outline_generation"] = "1"
        case .inheritedDocument: w["accepted_document_generation"] = "1"
        case .missingGeneration: w.removeValue(forKey: "accepted_document_generation")
        default: break
        }
        return json(w)
    }
    private func flow(_ handle: UInt64, generation: UInt64, width: UInt64) -> String {
        let state = record("flow")
        precondition(generation == 1 && width == AtlasParcelMarkdownWorker.flowWidth)
        var w = header("document-prepare", handle: handle)
        w["document_generation"] = state.fault == .flowGeneration ? "2" : String(generation)
        w["document_ready"] = true; w["layout_complete"] = state.fault != .flowIncomplete
        w["native_shaped"] = false; w["rendering"] = "logical-frankenmarkdown-flow"
        w["source_mapping"] = "enclosing-regions-not-glyph-exact"
        w["width_columns"] = String(width); w["total_flow_lines"] = "0"; w["total_headings"] = "0"
        w["rendered_utf8_bytes"] = "0"
        w["parser_source_base"] = vector.text.utf8.starts(with: [0xef, 0xbb, 0xbf]) ? "3" : "0"
        w["first_flow_line"] = "0"; w["flow_lines"] = [[String: Any]]()
        w["next_flow_line"] = NSNull(); w["whole_document_visible"] = true
        w["headings"] = [[String: Any]](); w["next_heading"] = NSNull()
        return json(w)
    }
    private func displayWire(_ handle: UInt64, command: String, first: UInt64, state: State) -> [String: Any] {
        var w = header(command, handle: handle)
        let total: UInt64 = vector.text.isEmpty ? 0 : 129
        let end = min(total, first + 128)
        let hasFont = state.fault == .missingFont || state.fault == .malformedFont
        w["document_generation"] = first > 0 && state.fault == .stalePage ? "2" : "1"
        w["display_width_points"] = state.fault == .displayWidth ? "641" : "640"
        w["rendering"] = "bundled-frankenmarkdown-display"; w["native_shaped"] = false; w["glyph_shaped"] = true
        w["source_mapping"] = "enclosing-ast-blocks-not-glyph-exact"
        w["source_ranges_qualified"] = state.fault != .unqualifiedDisplay
        w["total_display_items"] = state.fault == .displayLimit ? "8193" : String(total)
        w["unresolved_images"] = "0"
        w["display_bounds"] = ["x": 0.0, "y": 0.0, "width": 640.0,
            "height": Double(total) * 20 + (first > 0 && state.fault == .changedInventory ? 1 : 0)]
        w["fonts"] = hasFont ? [["font_index": "0", "byte_length": "4"]] : [[String: String]]()
        w["first_display_item"] = first > 0 && state.fault == .repeatedPage ? "0" : String(first)
        w["display_items"] = (first..<end).map { index -> [String: Any] in
            ["item_index": String(index), "kind": "vector",
             "bounds": ["x": 0.0, "y": Double(index) * 20, "width": 300.0, "height": 1.0],
             "enclosing_original_range": NSNull(), "shape": "horizontal-rule", "color_role": "rule", "stroke_width": 1.0]
        }
        w["next_display_item"] = end < total ? String(end) as Any : NSNull()
        if first > 0 && state.fault == .incompletePage { w["display_items"] = [[String: Any]]() }
        return w
    }
    private func display(_ handle: UInt64, generation: UInt64, width: UInt64) -> String {
        let state = record("display")
        precondition(generation == 1 && width == AtlasParcelMarkdownWorker.displayWidth)
        return json(displayWire(handle, command: "document-display-prepare", first: 0, state: state))
    }
    private func page(_ handle: UInt64, generation: UInt64, first: UInt64, count: UInt64) -> String? {
        let state = record("page")
        precondition(generation == 1 && first == 128 && count == AtlasDocumentDisplayLimits.pageItems)
        guard state.fault != .missingPage else { return nil }
        return json(displayWire(handle, command: "document-display-page", first: first, state: state))
    }
    private func font(_ handle: UInt64, generation: UInt64, index: UInt64, offset: UInt64, count: UInt64) -> String? {
        let state = record("font")
        precondition(generation == 1 && index == 0 && offset == 0 && count == AtlasDocumentDisplayLimits.fontChunk)
        guard state.fault != .missingFont else { return nil }
        var w = displayWire(handle, command: "document-display-font", first: 0, state: state)
        w["font_index"] = "0"; w["font_offset"] = "0"; w["font_bytes"] = "4"; w["total_font_bytes"] = "4"
        w["font_hex"] = "not-hex"; w["next_font_offset"] = NSNull()
        return json(w)
    }

    func transport() -> AtlasParcelMarkdownTransport {
        var document = AtlasReaderDocumentTransport(
            prepare: { [self] in flow($0, generation: $1, width: $2) },
            window: { _, _, _, _ in preconditionFailure("Unexpected logical window") },
            headings: { _, _, _, _ in preconditionFailure("Unexpected headings") },
            heading: { _, _, _, _ in preconditionFailure("Unexpected heading jump") },
            fromSource: { _, _, _, _ in preconditionFailure("Unexpected source jump") },
            source: { _, _, _, _, _ in preconditionFailure("Unexpected source reread") },
            copy: { _, _, _, _, _ in preconditionFailure("Unexpected copy") })
        document.prepareDisplay = { [self] in display($0, generation: $1, width: $2) }
        document.displayPage = { [self] in page($0, generation: $1, first: $2, count: $3) }
        document.displayFont = { [self] in font($0, generation: $1, index: $2, offset: $3, count: $4) }
        var transport = AtlasParcelMarkdownTransport(create: { [self] in
            _ = record("create")
            return change { let result = $0.next; $0.next += 1; return result }
        }, supply: { [self] handle, label, bytes in
            let state = record("supply")
            precondition(label.utf8.elementsEqual(self.label.utf8) && bytes == Data(vector.text.utf8),
                "Accepted captured bytes or raw path were changed before supply")
            change { $0.supplied.append(bytes) }
            return .init(json: sourceInfo(handle, fault: state.fault),
                sha256: state.fault == .expectedDigest ? String(repeating: "0", count: 64) : vector.sha256)
        }, close: { [self] handle in
            let state = record("close")
            change { $0.closed[handle, default: 0] += 1 }
            return state.closeAllowed
        }, document: document)
        transport.retirement = AtlasParcelMarkdownRetirement(maximumHandles: 1, retryPause: {})
        return transport
    }
}

@main enum AtlasParcelMarkdownWorkerTests {
    static func load(_ fixture: ParcelFixture, transport: AtlasParcelMarkdownTransport? = nil) throws -> AtlasParcelMarkdownPacket {
        try AtlasParcelMarkdownWorker.load(path: fixture.label, source: fixture.vector.text,
            transport: transport ?? fixture.transport(), canceled: { fixture.snapshot.canceled })
    }
    static func refused(_ fixture: ParcelFixture, transport: AtlasParcelMarkdownTransport? = nil) {
        do { _ = try load(fixture, transport: transport); preconditionFailure("Invalid/canceled packet was published") }
        catch { }
    }

    static func completePacketsKeepExactBytesAndRetire() throws {
        for vector in AtlasSuppliedReaderVector.exactCases {
            let fixture = ParcelFixture(vector), transport = fixture.transport()
            let packet = try load(fixture, transport: transport)
            let expectedItems = vector.text.isEmpty ? 0 : 129
            precondition(packet.captureSHA256 == vector.sha256 && packet.fonts.isEmpty)
            precondition(packet.pages.count == (expectedItems == 0 ? 1 : 2))
            precondition(packet.pages.flatMap(\.items).count == expectedItems)
            precondition(packet.pages.last?.next == nil && packet.pages.first?.first == 0)
            if expectedItems > 0 { precondition(packet.pages.last?.first == 128) }
            precondition(fixture.snapshot.supplied == [Data(vector.text.utf8)])
            precondition(fixture.snapshot.closed[1] == 1 && transport.retirement.reservedHandles == 0)
            precondition(fixture.snapshot.calls.last == "close", "A packet escaped before reader retirement")
        }
    }

    static func malformedRepliesNeverPublishPrefixes() {
        for fault in ParcelFixture.Fault.allCases where fault != .none {
            let fixture = ParcelFixture()
            fixture.change { $0.fault = fault }
            let transport = fixture.transport()
            refused(fixture, transport: transport)
            precondition(fixture.snapshot.closed[1] == 1 && transport.retirement.reservedHandles == 0,
                "Malformed packet leaked a supplied reader: \(fault)")
            switch fault {
            case .digest, .expectedDigest, .bytes, .path, .origin, .initialRead, .initialCalls,
                 .inheritedQuery, .inheritedOutline, .inheritedDocument, .missingGeneration:
                precondition(!fixture.snapshot.calls.contains("flow"), "Flow began before source validation: \(fault)")
            case .flowIncomplete, .flowGeneration:
                precondition(!fixture.snapshot.calls.contains("display"), "Styled work began before logical inventory validation")
            default: break
            }
        }
    }

    static func cancellationRetiresAtEveryStage() {
        for stage in ["create", "supply", "flow", "display", "page", "font", "close"] {
            let fixture = ParcelFixture()
            fixture.change { $0.cancelAt = stage; if stage == "font" { $0.fault = .malformedFont } }
            let transport = fixture.transport()
            refused(fixture, transport: transport)
            precondition(fixture.snapshot.canceled && fixture.snapshot.closed[1] == 1)
            precondition(transport.retirement.reservedHandles == 0 && transport.retirement.quarantinedHandles == 0)
            let calls = fixture.snapshot.calls
            if let index = calls.firstIndex(of: stage), stage != "close" {
                precondition(calls[(index + 1)...].allSatisfy { $0 == "close" },
                    "Cancellation continued document demand after \(stage)")
            }
        }
        let before = ParcelFixture()
        before.change { $0.canceled = true }
        refused(before)
        precondition(before.snapshot.calls.isEmpty, "Pre-canceled work allocated a reader")
    }

    static func admissionPrecedesSourceAllocation() {
        for path in ["", "bad\0path", String(repeating: "x", count: 16_385)] {
            let fixture = ParcelFixture(), transport = fixture.transport()
            do {
                _ = try AtlasParcelMarkdownWorker.load(path: path, source: fixture.vector.text, transport: transport)
                preconditionFailure("Invalid label was accepted")
            } catch { }
            precondition(fixture.snapshot.calls.isEmpty && transport.retirement.reservedHandles == 0)
        }
        let fixture = ParcelFixture(), transport = fixture.transport()
        do {
            _ = try AtlasParcelMarkdownWorker.load(path: fixture.label,
                source: String(repeating: "x", count: AtlasParcelMarkdownWorker.sourceBytes + 1), transport: transport)
            preconditionFailure("Oversized capture was truncated or supplied")
        } catch { }
        precondition(fixture.snapshot.calls.isEmpty && transport.retirement.reservedHandles == 0)
    }

    static func failedRetirementHoldsAdmissionAndSuppressesPacket() throws {
        let fixture = ParcelFixture(), transport = fixture.transport()
        fixture.change { $0.closeAllowed = false }
        refused(fixture, transport: transport)
        precondition(fixture.snapshot.closed[1] == 8 && transport.retirement.reservedHandles == 1)
        precondition(transport.retirement.quarantinedHandles == 1)
        refused(fixture, transport: transport)
        precondition(fixture.snapshot.next == 2 && fixture.snapshot.supplied.count == 1,
            "A full retirement quarantine admitted another capture")
        fixture.change { $0.closeAllowed = true }
        let packet = try load(fixture, transport: transport)
        precondition(packet.pages.count == 2 && fixture.snapshot.next == 3)
        precondition(fixture.snapshot.closed[1] == 17 && fixture.snapshot.closed[2] == 1)
        precondition(transport.retirement.reservedHandles == 0 && transport.retirement.quarantinedHandles == 0)
    }

    static func main() throws {
        try completePacketsKeepExactBytesAndRetire()
        malformedRepliesNeverPublishPrefixes()
        cancellationRetiresAtEveryStage()
        admissionPrecedesSourceAllocation()
        try failedRetirementHoldsAdmissionAndSuppressesPacket()
        print("Parcel Markdown worker: complete packets, exact captures, hostile receipts, cancellation and bounded retirement passed")
    }
}
