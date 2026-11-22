import Foundation

// Only registry-domain dependencies are stubbed. The desk's actual C adapter,
// bounded owned-string copy and public header definitions are linked below.
struct AtlasReaderTransport: Sendable {
    struct Comparison: Sendable { let domain = UUID() }
    let comparison: Comparison? = Comparison()
    static let native = Self()
}
enum AtlasObservedReaderTransport { static let nativeDomain = UUID() }

@_silgen_name("desk_observed_fixture_reset") private func reset()
@_silgen_name("desk_observed_fixture_mode") private func mode(_ value: UInt64)
@_silgen_name("desk_observed_fixture_argument") private func argument(_ index: UInt64) -> UInt64
@_silgen_name("desk_observed_fixture_calls") private func calls() -> UInt64
@_silgen_name("desk_observed_fixture_frees") private func frees() -> UInt64
@_silgen_name("desk_observed_fixture_body_length") private func bodyLength() -> UInt64
@_silgen_name("desk_observed_fixture_body_byte") private func bodyByte(_ index: UInt64) -> UInt64
@_silgen_name("desk_observed_fixture_label_byte") private func labelByte(_ index: UInt64) -> UInt64

@main enum AtlasDeskObservedBridgeTests {
    static func main() {
        let observed = AtlasReadingDeskTransport.native.observed!
        precondition(observed.domain == AtlasObservedReaderTransport.nativeDomain)
        let h: UInt64 = 0x8000000000000001, r = h + 1, a = h + 2, reader = h + 3, o = h + 4
        let start: UInt64 = (1 << 40) + 7, end = start + 256
        reset()
        precondition(observed.keep(h, r, a, reader, o, start, end) == "{}")
        for (index, value) in [h, r, a, reader, o, start, end].enumerated() {
            precondition(argument(UInt64(index)) == value)
        }
        precondition(calls() == 1 && frees() == 1)
        precondition(observed.read(h, r, o) == "{}")
        precondition(argument(0) == h && argument(1) == r && argument(2) == o && frees() == 2)
        let body = "x\0😀\r\ne\u{301}", label = "saved 😀"
        precondition(observed.edit(h, r, a, o, h + 5, label, body) == "{}")
        precondition(argument(4) == h + 5 && bodyLength() == UInt64(body.utf8.count))
        for (index, value) in body.utf8.enumerated() { precondition(bodyByte(UInt64(index)) == UInt64(value)) }
        for (index, value) in label.utf8.enumerated() { precondition(labelByte(UInt64(index)) == UInt64(value)) }
        precondition(observed.forget(h, r, a, o, h + 6) == "{}")
        precondition(argument(4) == h + 6 && calls() == 4 && frees() == 4)
        precondition(observed.edit(h, r, a, o, 1, "empty body", "") == "{}" && bodyLength() == 0)

        let before = calls()
        precondition(observed.keep(h, r, a, reader, o, end, start) == nil)
        precondition(observed.keep(h, r, a, reader, o, start, start) == nil)
        precondition(observed.keep(h, r, a, reader, o, start, start + 262_145) == nil)
        precondition(observed.edit(h, r, a, o, 1, "", body) == nil)
        precondition(observed.edit(h, r, a, o, 1, "nul\0label", body) == nil)
        precondition(observed.edit(h, r, a, o, 1, "label", String(repeating: "x", count: 16_385)) == nil)
        precondition(calls() == before, "Rejected inputs never cross the ABI")

        reset(); mode(1)
        precondition(observed.read(h, r, o) == nil && calls() == 1 && frees() == 0)
        mode(2)
        precondition(observed.read(h, r, o) == nil && frees() == 1, "Malformed UTF-8 is freed once")
        mode(3)
        precondition(observed.read(h, r, o) == nil && frees() == 2, "Oversized framing is freed once")
        mode(4)
        precondition(observed.read(h, r, o)?.utf8.count == 5 * 1024 * 1024 && frees() == 3,
            "The shared bridge framing admits large state delivery; command-specific decoding applies its own limit")
        print("Observed passage native C ABI and owned reply regressions passed")
    }
}
