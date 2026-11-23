import Foundation
import CoreGraphics
import CryptoKit
import Dispatch

/// Artifact bytes only: production codecs, CoreText preparation, source
/// validation and viewport admission still run. This is not a Rust-cache or
/// native-window performance result.
private final class ViewportArtifactFixture: @unchecked Sendable {
    private let lock = NSLock()
    private var text: String
    private var role = "plain"
    private var forcedKey: [UInt8]?
    private var environment = "fixture-font-environment-1"
    private var values: [String: Data] = [:]
    private var captures = 0, reads = 0, writes = 0, repairs = 0
    private var gate: DispatchSemaphore?
    private var entered = false
    private var refuseWrites = false

    init(text: String = "let exact_capture = 41;\nlet next_capture = 42;\n") { self.text = text }
    var snapshot: (captures: Int, reads: Int, writes: Int, repairs: Int, entries: Int, entered: Bool) {
        lock.lock(); defer { lock.unlock() }; return (captures, reads, writes, repairs, values.count, entered)
    }
    func changeText(_ value: String, retainingKey: Bool = false, role nextRole: String = "plain") {
        lock.lock(); defer { lock.unlock() }
        forcedKey = retainingKey ? Self.sourceKey(text: text, role: role) : nil
        text = value; role = nextRole
    }
    func changeEnvironment() { lock.lock(); environment += "-changed"; lock.unlock() }
    func denyWrites() { lock.lock(); refuseWrites = true; lock.unlock() }
    func blockNextRead() -> DispatchSemaphore {
        lock.lock(); defer { lock.unlock() }
        let value = DispatchSemaphore(value: 0); gate = value; entered = false; return value
    }
    func corrupt(_ marker: String) {
        lock.lock(); defer { lock.unlock() }
        let keys = values.compactMap { key, value -> String? in
            var reader = AtlasBinaryReader(data: value)
            return (try? reader.string(16)) == marker ? key : nil
        }
        precondition(!keys.isEmpty)
        for key in keys { values[key] = Data("corrupt-native-artifact".utf8) }
    }
    func discard(_ marker: String) {
        lock.lock(); defer { lock.unlock() }
        let keys = values.compactMap { key, value -> String? in
            var reader = AtlasBinaryReader(data: value)
            return (try? reader.string(16)) == marker ? key : nil
        }
        precondition(!keys.isEmpty)
        for key in keys { values.removeValue(forKey: key) }
    }
    private static func sourceKey(text: String, role: String) -> [UInt8] {
        var hash = SHA256(); hash.update(data: Data(text.utf8)); hash.update(data: Data(role.utf8))
        return Array(hash.finalize())
    }
    var transport: AtlasViewportSourceTransport {
        AtlasViewportSourceTransport(load: { [self] _, handle, _ in
            precondition(!Thread.isMainThread && handle == 17)
            lock.lock(); captures += 1
            let currentText = text, currentRole = role
            let key = forcedKey ?? Self.sourceKey(text: currentText, role: currentRole)
            lock.unlock()
            let runs: [[String: String]] = currentText.isEmpty ? []
                : [["start": "0", "length": String(currentText.utf16.count), "role": currentRole]]
            return AtlasProjectSourcePacket(json: try JSONSerialization.data(withJSONObject:
                ["schema": "fcb.source-document/1", "text": currentText, "runs": runs]), key: key)
        }, artifacts: AtlasViewportArtifactTransport(environment: { [self] cancellation in
            precondition(!Thread.isMainThread)
            if cancellation.isCanceled { throw AtlasPreparationError.canceled }
            lock.lock(); defer { lock.unlock() }; return environment
        }, load: { [self] handle, key, limit, _ in
            precondition(!Thread.isMainThread && handle == 17 && limit == 16 * 1024 * 1024)
            lock.lock(); reads += 1
            let value = values[key], pending = gate
            if pending != nil { gate = nil; entered = true }
            lock.unlock()
            if let pending { precondition(pending.wait(timeout: .now() + 5) == .success) }
            // A late completed read is deliberate: production must independently
            // reject it after cancellation/root replacement/environment change.
            return value
        }, store: { [self] handle, key, data, repair, cancellation in
            precondition(!Thread.isMainThread && handle == 17 && data.count <= 16 * 1024 * 1024)
            if cancellation.isCanceled { throw AtlasPreparationError.canceled }
            lock.lock(); defer { lock.unlock() }
            if refuseWrites { return false }
            writes += 1; if repair { repairs += 1 }
            if values[key] == nil || repair { values[key] = data }
            return true
        }))
    }
}

private final class ViewportCacheLease {}

extension AtlasViewportResidencyTests {
    private static func cacheModel(_ fixture: ViewportArtifactFixture, file: AtlasProjectCatalog.Entry,
        root: String = "/fixture", limits: AtlasViewportLimits = AtlasViewportLimits(),
        lease: AnyObject? = nil) -> AtlasViewportResidency {
        let model = AtlasViewportResidency(limits: limits, transport: fixture.transport)
        precondition(model.install(root: root, entries: [file], layoutRevision: UUID(), accessLease: nil,
            sourceCacheHandle: 17, cacheLease: lease ?? fixture))
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        return model
    }
    private static func closeCacheModel(_ model: AtlasViewportResidency) async throws {
        model.close(); try await until { model.retainedNativeSlots == 0 }
    }
    private static func cachePixels(_ model: AtlasViewportResidency, file: AtlasProjectCatalog.Entry) -> [Data] {
        let tiles = model.document(forRawPath: file.rawPath)!.renderTiles
        return tiles.map { Data(($0.raster!.dataProvider!.data!) as Data) }
    }
    private static func cacheGlyphs(_ model: AtlasViewportResidency, file: AtlasProjectCatalog.Entry) -> [UInt64] {
        var result: [UInt64] = []
        for tile in model.document(forRawPath: file.rawPath)!.tiles {
            for line in tile.preparedLines ?? [] {
                result += [UInt64(line.sourceRange.location), UInt64(line.sourceRange.length)]
                for run in line.runs {
                    for (glyph, point) in zip(run.glyphs, run.positions) {
                        result += [UInt64(glyph), Double(point.x).bitPattern, Double(point.y).bitPattern]
                    }
                }
            }
        }
        return result
    }

    static func preparedArtifactsSurviveWarmReopen() async throws {
        let fixture = ViewportArtifactFixture(), file = entry(1, "cached.rs")
        let cold = cacheModel(fixture, file: file)
        try await ready(cold, file)
        let first = cold.preparation(forRawPath: file.rawPath)!
        precondition(first.shapedDocuments == 1 && first.rasterizedTiles > 0 && first.artifactsWritten == 2)
        precondition(!first.glyphArchiveHit && !first.rasterArchiveHit)
        let pixels = cachePixels(cold, file: file), glyphs = cacheGlyphs(cold, file: file)
        let source = Array(cold.document(forRawPath: file.rawPath)!.source.text.utf8)
        try await closeCacheModel(cold)

        let warm = cacheModel(fixture, file: file)
        try await ready(warm, file)
        let second = warm.preparation(forRawPath: file.rawPath)!
        precondition(second.glyphArchiveHit && second.rasterArchiveHit)
        precondition(second.shapedDocuments == 0 && second.restoredColorLines == 0 && second.rasterizedTiles == 0)
        precondition(second.artifactsWritten == 0 && fixture.snapshot.captures == 2,
            "a native hit still obtains the authoritative current Rust source capture")
        precondition(cachePixels(warm, file: file) == pixels && cacheGlyphs(warm, file: file) == glyphs)
        precondition(Array(warm.document(forRawPath: file.rawPath)!.source.text.utf8) == source)
        try await closeCacheModel(warm)
    }

    static func preparedArtifactsInvalidateExactDependencies() async throws {
        let original = "let exact_capture = 41;\n", changed = "let exact_capture = 42;\n"
        precondition(original.utf8.count == changed.utf8.count)
        let fixture = ViewportArtifactFixture(text: original), file = entry(1, "same.rs")
        let cold = cacheModel(fixture, file: file); try await ready(cold, file); try await closeCacheModel(cold)
        fixture.changeText(changed)
        let mutation = cacheModel(fixture, file: file); try await ready(mutation, file)
        let mutated = mutation.preparation(forRawPath: file.rawPath)!
        precondition(mutated.shapedDocuments == 1 && !mutated.glyphArchiveHit && !mutated.rasterArchiveHit)
        precondition(mutation.document(forRawPath: file.rawPath)!.source.text.utf8.elementsEqual(changed.utf8))
        try await closeCacheModel(mutation)

        let otherRoot = cacheModel(fixture, file: file, root: "/other-root"); try await ready(otherRoot, file)
        precondition(otherRoot.preparation(forRawPath: file.rawPath)!.shapedDocuments == 1,
            "identical path/content in another root has a separate native artifact identity")
        try await closeCacheModel(otherRoot)
        fixture.changeEnvironment()
        let newFonts = cacheModel(fixture, file: file); try await ready(newFonts, file)
        precondition(newFonts.preparation(forRawPath: file.rawPath)!.shapedDocuments == 1)
        precondition(!newFonts.preparation(forRawPath: file.rawPath)!.rasterArchiveHit)
        try await closeCacheModel(newFonts)
    }

    static func preparedArtifactCorruptionAndCaptureMismatchFallBack() async throws {
        let fixture = ViewportArtifactFixture(text: "let captured = 11;\n"), file = entry(1, "exact.rs")
        let cold = cacheModel(fixture, file: file); try await ready(cold, file)
        let pixels = cachePixels(cold, file: file)
        try await closeCacheModel(cold)
        fixture.corrupt("FCBPT5")
        let glyphRepair = cacheModel(fixture, file: file); try await ready(glyphRepair, file)
        precondition(glyphRepair.preparation(forRawPath: file.rawPath)!.shapedDocuments == 1)
        precondition(!glyphRepair.preparation(forRawPath: file.rawPath)!.rasterArchiveHit,
            "an invalid glyph dependency also withdraws its associated rasters")
        precondition(cachePixels(glyphRepair, file: file) == pixels && fixture.snapshot.repairs == 2)
        try await closeCacheModel(glyphRepair)
        fixture.corrupt("FCBOV2")
        let rasterRepair = cacheModel(fixture, file: file); try await ready(rasterRepair, file)
        let repaired = rasterRepair.preparation(forRawPath: file.rawPath)!
        precondition(repaired.glyphArchiveHit && repaired.shapedDocuments == 0 && repaired.rasterizedTiles > 0)
        precondition(cachePixels(rasterRepair, file: file) == pixels && fixture.snapshot.repairs == 3)
        try await closeCacheModel(rasterRepair)

        fixture.discard("FCBPT5")
        let missingGlyphs = cacheModel(fixture, file: file); try await ready(missingGlyphs, file)
        let rebuilt = missingGlyphs.preparation(forRawPath: file.rawPath)!
        precondition(rebuilt.shapedDocuments == 1 && !rebuilt.rasterArchiveHit && rebuilt.rasterizedTiles > 0,
            "pixels without a validated glyph/font dependency must be regenerated")
        precondition(fixture.snapshot.repairs == 4 && cachePixels(missingGlyphs, file: file) == pixels)
        try await closeCacheModel(missingGlyphs)

        // Deliberately violate the packet/key association to emulate a
        // checksummed but miskeyed native artifact. Exact current text wins.
        fixture.changeText("let captured = 22;\n", retainingKey: true)
        let mismatch = cacheModel(fixture, file: file); try await ready(mismatch, file)
        let rejected = mismatch.preparation(forRawPath: file.rawPath)!
        precondition(rejected.shapedDocuments == 1 && !rejected.glyphArchiveHit && !rejected.rasterArchiveHit)
        precondition(mismatch.document(forRawPath: file.rawPath)!.source.text == "let captured = 22;\n")
        precondition(cachePixels(mismatch, file: file) != pixels && fixture.snapshot.repairs == 6)
        try await closeCacheModel(mismatch)
    }

    static func preparedArtifactsReflowAndRasterBudgetAreDistinct() async throws {
        let source = (0..<180).map { "let source_\($0) = \($0);\n" }.joined()
        let fixture = ViewportArtifactFixture(text: source), file = entry(1, "many.rs", width: 160, height: 120)
        let cold = cacheModel(fixture, file: file); try await ready(cold, file); try await closeCacheModel(cold)
        let wide = entry(1, "many.rs", width: 700, height: 90)
        let refitted = cacheModel(fixture, file: wide); try await ready(refitted, wide)
        let fitted = refitted.preparation(forRawPath: wide.rawPath)!
        precondition(fitted.glyphArchiveHit && fitted.shapedDocuments == 0 && !fitted.rasterArchiveHit && fitted.rasterizedTiles > 0)
        precondition(refitted.tiles.allSatisfy { $0.parcelRect == rect(wide) })
        try await closeCacheModel(refitted)

        var limits = AtlasViewportLimits(); limits.pixels = 2048
        let reduced = cacheModel(fixture, file: file, limits: limits); try await ready(reduced, file)
        let receipt = reduced.preparation(forRawPath: file.rawPath)!
        precondition(receipt.glyphArchiveHit && receipt.shapedDocuments == 0 && !receipt.rasterArchiveHit)
        precondition(reduced.residentCost.pixels <= 2048 && receipt.rasterizedTiles > 0)
        try await closeCacheModel(reduced)
    }

    static func preparedArtifactCancellationRetainsEnteredLease() async throws {
        let fixture = ViewportArtifactFixture(), file = entry(1, "held.rs")
        let cold = cacheModel(fixture, file: file); try await ready(cold, file); try await closeCacheModel(cold)
        let gate = fixture.blockNextRead()
        var lease: ViewportCacheLease? = ViewportCacheLease()
        weak var observedLease = lease
        let canceled = cacheModel(fixture, file: file, lease: lease)
        lease = nil
        try await until { fixture.snapshot.entered }
        canceled.close()
        precondition(observedLease != nil && canceled.documents.isEmpty)
        gate.signal()
        try await until { observedLease == nil && canceled.retainedNativeSlots == 0 }
        precondition(canceled.documents.isEmpty && fixture.snapshot.writes == 2,
            "a canceled cache read cannot publish a document or a replacement artifact")
    }

    static func preparedFontChangeRejectsEnteredCandidate() async throws {
        let fixture = ViewportArtifactFixture(), file = entry(1, "fonts.rs")
        let cold = cacheModel(fixture, file: file); try await ready(cold, file); try await closeCacheModel(cold)
        let gate = fixture.blockNextRead(), changing = cacheModel(fixture, file: file)
        try await until { fixture.snapshot.entered }
        fixture.changeEnvironment(); gate.signal()
        try await ready(changing, file)
        precondition(fixture.snapshot.captures == 3,
            "a completed old-font candidate is rejected and latest demand is prepared again")
        precondition(changing.preparation(forRawPath: file.rawPath)!.shapedDocuments == 1)
        try await closeCacheModel(changing)
    }

    static func preparedCacheRefusalAndDecoderBudgetsPreserveSource() async throws {
        let fixture = ViewportArtifactFixture(), file = entry(1, "quota.rs")
        fixture.denyWrites()
        for _ in 0..<2 {
            let model = cacheModel(fixture, file: file); try await ready(model, file)
            let receipt = model.preparation(forRawPath: file.rawPath)!
            precondition(receipt.shapedDocuments == 1 && receipt.artifactsWritten == 0
                && model.previewStates[file.id] == .ready)
            try await closeCacheModel(model)
        }
        precondition(fixture.snapshot.entries == 0)

        let text = "let exact = 1;\n"
        let capture = AtlasHighlightCapture(schema: "fcb.source-document/1", text: text,
            runs: [.init(start: "0", length: String(text.utf16.count), role: "plain")])
        let document = AtlasDocument(path: "limits.rs", capture: capture)!
        let key = String(repeating: "a", count: 64), fonts = AtlasPreparedFonts()
        let data = try AtlasDocumentArchive.encode(document, key: key, fonts: fonts, includeRasters: false)
        func refused(_ action: () throws -> Void) {
            do { try action(); preconditionFailure("bounded glyph archive unexpectedly accepted") }
            catch { }
        }
        refused { _ = try AtlasDocumentArchive.decode(data, path: "limits.rs", key: key, fonts: fonts,
            includeRasters: false, byteLimit: data.count - 1, expectedCapture: capture) }
        refused { _ = try AtlasDocumentArchive.decode(data, path: "limits.rs", key: key, fonts: fonts,
            includeRasters: false, captureRunLimit: 0, expectedCapture: capture) }
        refused { _ = try AtlasDocumentArchive.decode(data, path: "limits.rs", key: key, fonts: fonts,
            includeRasters: false, tileLimit: 0, expectedCapture: capture) }
        refused { _ = try AtlasDocumentArchive.decode(data, path: "limits.rs", key: key, fonts: fonts,
            glyphLimit: 0, includeRasters: false, expectedCapture: capture) }
        refused { _ = try AtlasDocumentArchive.decode(data, path: "limits.rs", key: key, fonts: fonts,
            includeRasters: false, expectedCapture: capture, canceled: { true }) }
        let differentRoles = AtlasHighlightCapture(schema: capture.schema, text: capture.text,
            runs: [.init(start: "0", length: String(text.utf16.count), role: "comment")])
        refused { _ = try AtlasDocumentArchive.decode(data, path: "limits.rs", key: key, fonts: fonts,
            includeRasters: false, expectedCapture: differentRoles) }
    }
}
