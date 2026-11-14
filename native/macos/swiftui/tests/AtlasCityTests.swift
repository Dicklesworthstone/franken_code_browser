import Foundation
import Dispatch

/// Injected wire replies exercise the REAL serial worker and main-actor model.
/// The fixture deliberately does not implement projection or render a drawable;
/// shared Rust tests and the separate physical-Mac lane own those oracles.
private final class CityFixture: @unchecked Sendable {
    private let lock = NSLock()
    private var creates = 0, opens = 0, infos = 0, views = 0, presents = 0, picks = 0, closes = 0
    private var width = 300.0, height = 100.0
    private var acknowledged: (generation: UInt64, frame: UInt64, display: UInt64) = (0, 0, 0)
    private var blockedGeneration: UInt64?
    private var blockedOpen = false
    let gate = DispatchSemaphore(value: 0)
    let lostOpen: Bool, lostPresent: Bool, wrongOwner: Bool
    init(lostOpen: Bool = false, lostPresent: Bool = false, wrongOwner: Bool = false) {
        self.lostOpen = lostOpen; self.lostPresent = lostPresent; self.wrongOwner = wrongOwner
    }
    struct Counts { let creates, opens, infos, views, presents, picks, closes: Int }
    var counts: Counts {
        lock.lock(); defer { lock.unlock() }
        return Counts(creates: creates, opens: opens, infos: infos, views: views,
            presents: presents, picks: picks, closes: closes)
    }
    func holdView(_ generation: UInt64) { lock.lock(); blockedGeneration = generation; lock.unlock() }
    func holdOpen() { lock.lock(); blockedOpen = true; lock.unlock() }
    var transport: AtlasCityTransport {
        AtlasCityTransport(create: { [self] in
            precondition(!Thread.isMainThread)
            lock.lock(); creates += 1; let handle = UInt64(70 + creates); lock.unlock()
            return handle
        }, open: { [self] handle, layout, parcels, complete in
            precondition(!Thread.isMainThread)
            precondition(layout == 1 && parcels.count == 2 && complete)
            lock.lock(); opens += 1; let ordinal = opens, block = blockedOpen; lock.unlock()
            if block { precondition(gate.wait(timeout: .now() + 5) == .success, "open gate deadline") }
            if lostOpen && ordinal == 1 { return nil }
            return Self.scene(owner: wrongOwner ? handle + 1 : handle)
        }, request: { [self] handle, command in
            precondition(!Thread.isMainThread)
            switch command {
            case .info:
                lock.lock(); infos += 1; lock.unlock()
                return Self.scene(owner: wrongOwner ? handle + 1 : handle)
            case .present(let generation, let frame, let display):
                lock.lock(); presents += 1; let ordinal = presents
                acknowledged = (generation, frame, display); lock.unlock()
                if lostPresent && ordinal == 1 { return nil }
                return Self.present(owner: handle, generation: generation, frame: frame, display: display)
            case .pick(let frame, let display, _, _):
                lock.lock(); picks += 1; let accepted = acknowledged; lock.unlock()
                precondition(accepted.frame == frame && accepted.display == display)
                return Self.pick(owner: handle, generation: accepted.generation, frame: frame, display: display)
            default:
                let generation: UInt64
                lock.lock()
                switch command {
                case .view(let g, let w, let h, _), .fit(let g, let w, let h):
                    generation = g; width = w; height = h
                case .resize(let g, _, let w, let h): generation = g; width = w; height = h
                case .pan(let g, _, _, _), .orbit(let g, _, _, _), .zoom(let g, _, _, _, _): generation = g
                case .info, .present, .pick: preconditionFailure("handled above")
                }
                views += 1
                let w = width, h = height, block = generation == blockedGeneration
                lock.unlock()
                if block { precondition(gate.wait(timeout: .now() + 5) == .success, "view gate deadline") }
                return Self.view(owner: handle, generation: generation, width: w, height: h)
            }
        }, close: { [self] _ in
            precondition(!Thread.isMainThread)
            lock.lock(); closes += 1; lock.unlock(); return true
        })
    }
    private static func json(_ fields: [String: Any]) -> String {
        String(data: try! JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys]), encoding: .utf8)!
    }
    static func scene(owner: UInt64 = 71) -> String {
        json(["schema": "fcb.city/1", "status": "ok", "command": "open", "owner": String(owner), "layout": "1",
            "metric": "capped-log-observed-bytes", "metric_cap_bytes": "1073741824", "maximum_height": 36.0,
            "origin": [150.0, 50.0], "file_count": "2", "unknown_count": "0", "capped_count": "0",
            "discovery_complete": true, "source_payload_read": false, "native_presented": false,
            "instances": [
                ["file_id": "91", "rect": [-150.0, -50.0, 100.0, 100.0], "height": 12.0, "observed_bytes": "1024"],
                ["file_id": "92", "rect": [50.0, -50.0, 100.0, 100.0], "height": 13.0, "observed_bytes": "2048"],
            ]])
    }
    static func view(owner: UInt64 = 71, generation: UInt64, width: Double = 300, height: Double = 100) -> String {
        json(["schema": "fcb.city/1", "status": "ok", "command": "view", "owner": String(owner), "layout": "1",
            "generation": String(generation), "projection": "orthographic", "width": width, "height": height,
            "matrix": [1.0,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1],
            "camera": ["center_x": 150.0, "center_y": 50.0, "scale": 1.0, "yaw": 0.0, "tilt": 0.7],
            "native_presented": false, "source_payload_read": false])
    }
    static func present(owner: UInt64 = 71, generation: UInt64, frame: UInt64, display: UInt64) -> String {
        json(["schema": "fcb.city/1", "status": "ok", "command": "present", "owner": String(owner), "layout": "1",
            "generation": String(generation), "frame": String(frame), "display": String(display),
            "native_presented": false, "acknowledgment": "host-declared", "source_payload_read": false])
    }
    static func pick(owner: UInt64 = 71, generation: UInt64, frame: UInt64, display: UInt64) -> String {
        json(["schema": "fcb.city/1", "status": "ok", "command": "pick", "owner": String(owner), "layout": "1",
            "generation": String(generation), "frame": String(frame), "display": String(display),
            "picking": "frontmost-cuboid-not-source-location", "file_id": "91", "source_payload_read": false])
    }
}

@MainActor @main struct AtlasCityTests {
    static var entries: [AtlasProjectCatalog.Entry] {
        [entry(91, "first.rs", x: 0, bytes: 1024), entry(92, "second.rs", x: 200, bytes: 2048)]
    }
    static func entry(_ id: UInt64, _ path: String, x: Double, bytes: UInt64) -> AtlasProjectCatalog.Entry {
        .init(id: id, rawPath: Array(path.utf8), displayPath: path, sourcePath: path,
            observedBytes: bytes, x: x, y: 0, width: 100, height: 100)
    }
    static func until(_ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(5)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 1_000_000) }
        precondition(condition(), "City fixture deadline; no missing callback is a pass")
    }
    static func make(_ fixture: CityFixture) -> AtlasCityModel {
        let model = AtlasCityModel(revision: UUID(), entries: entries, complete: true,
            seed: AtlasCitySeed(scale: 1, offsetX: 0, offsetY: 0), transport: fixture.transport)
        model.resize(width: 300, height: 100, backing: 1); model.start()
        return model
    }
    static func finish(_ model: inout AtlasCityModel?) async throws {
        model?.close(); model = nil
        try await until { AtlasCityLifetime.scenes.reservedSlots == 0 }
    }
    static func acknowledge(_ model: AtlasCityModel, frame: UInt64) async throws {
        precondition(model.frame != nil)
        model.presented(.init(view: model.frame!, frame: frame, display: model.display))
        try await until { model.canPick && !model.busy }
    }
    static func main() async throws {
        let retained = AtlasCityCamera(center_x: 701, center_y: 902, scale: 2.5, yaw: 1.2, tilt: 0.45)
        let refreshSeed = AtlasCitySeed(scale: 0.2, offsetX: 17, offsetY: 31, retainedCamera: retained)
        precondition(refreshSeed.camera(width: 400, height: 200) == retained,
            "compatible refresh keeps the displayed City's center, zoom, orbit and tilt")
        precondition(AtlasCitySeed(scale: 2, offsetX: 0, offsetY: 0).camera(width: 400, height: 200)
            == AtlasCityCamera(center_x: 100, center_y: 50, scale: 2, yaw: 0, tilt: 0.7),
            "ordinary City opening still derives its camera from the 2D atlas")
        try wireIdentityAndRefusal()
        try await lostReplyRecoveryAndAcceptedSelection()
        try await delayedOldStyleAcknowledgmentCannotRestorePicking()
        try await resizeAndCloseFenceLateWork()
        try await canceledOpenRetiresItsSceneAdmission()
        try await globalSceneAdmissionAndRetry()
        try await wrongOwnerCannotPublish()
        print("City: 7 wire/worker/model regression scenarios passed. No GPU pixels or physical-Mac qualification claimed.")
    }

    static func wireIdentityAndRefusal() throws {
        let parcels = entries.map(AtlasCityParcel.init)
        let scene = try AtlasCityScene.decode(CityFixture.scene(), parcels: parcels, layout: 1, complete: true, owner: 71)
        precondition(scene.instances.map(\.id) == [91, 92] && scene.originX == 150)
        let frame = try AtlasCityFrame.decode(CityFixture.view(generation: 1), scene: scene,
            generation: 1, width: 300, height: 100)
        let receipt = AtlasCityReceipt(view: frame, frame: 8, display: 2)
        try receipt.acknowledge(CityFixture.present(generation: 1, frame: 8, display: 2))
        let picked = try receipt.pick(CityFixture.pick(generation: 1, frame: 8, display: 2))
        precondition(picked == 91)
        func refuses(_ work: () throws -> Void) {
            do { try work(); preconditionFailure("negative control accepted an incompatible City receipt") }
            catch { }
        }
        refuses { _ = try AtlasCityScene.decode(CityFixture.scene(owner: 72), parcels: parcels, layout: 1, complete: true, owner: 71) }
        refuses { _ = try AtlasCityScene.decode(CityFixture.scene(), parcels: Array(parcels.reversed()), layout: 2, complete: true, owner: 71) }
        refuses { _ = try AtlasCityFrame.decode(CityFixture.view(generation: 2), scene: scene, generation: 1, width: 300, height: 100) }
        refuses { try receipt.acknowledge(CityFixture.present(generation: 1, frame: 8, display: 3)) }
        refuses { _ = try receipt.pick(CityFixture.pick(generation: 2, frame: 8, display: 2)) }
        precondition(AtlasCityMotion.pan(2, 3).coalescing(.pan(5, -1)) == .pan(7, 2))
        precondition(AtlasCityMotion.zoom(50, 60, 2).coalescing(.zoom(50, 60, 3)) == .zoom(50, 60, 6))
    }

    static func lostReplyRecoveryAndAcceptedSelection() async throws {
        let fixture = CityFixture(lostOpen: true, lostPresent: true)
        var model: AtlasCityModel? = make(fixture)
        try await until { model?.frame?.generation == 1 && model?.busy == false }
        precondition(fixture.counts.opens == 1 && fixture.counts.infos == 1)
        var selected: UInt64?
        model?.onSelect = { selected = $0.id }
        model?.click(x: 50, y: 50)
        precondition(fixture.counts.picks == 0 && model?.canPick == false)
        try await acknowledge(model!, frame: 1)
        precondition(fixture.counts.presents == 2, "lost presentation reply replays the same receipt once")
        model?.click(x: 50, y: 50)
        try await until { selected == 91 && model?.busy == false }
        precondition(fixture.counts.picks == 1)
        model?.selectPath("first.rs"); model?.readAdjacent(1)
        precondition(selected == 92, "keyboard navigation opens the same retained metadata IDs")
        try await finish(&model)
        try await until { fixture.counts.closes == 1 }
    }

    static func delayedOldStyleAcknowledgmentCannotRestorePicking() async throws {
        let fixture = CityFixture()
        var model: AtlasCityModel? = make(fixture)
        try await until { model?.frame?.generation == 1 && model?.busy == false }
        try await acknowledge(model!, frame: 1)
        let old = model!.frame!, display = model!.display
        fixture.holdView(2)
        model?.move(.pan(50, 0))
        try await until { fixture.counts.views == 2 }
        // A style-only redraw of A arrives while camera B is on the worker.
        model?.presented(.init(view: old, frame: 2, display: display))
        fixture.gate.signal()
        try await until { model?.frame?.generation == 2 && model?.busy == false }
        precondition(model?.canPick == false, "old style acknowledgment must not restore A after B publication")
        model?.click(x: 50, y: 50)
        precondition(fixture.counts.picks == 0)
        model?.presented(.init(view: old, frame: 2, display: display))
        precondition(model?.canPick == false, "late duplicate old drawable cannot become current")
        try await acknowledge(model!, frame: 3)
        var selected: UInt64?
        model?.onSelect = { selected = $0.id }; model?.click(x: 50, y: 50)
        try await until { selected == 91 }
        try await finish(&model)
    }

    static func resizeAndCloseFenceLateWork() async throws {
        let fixture = CityFixture()
        var model: AtlasCityModel? = make(fixture)
        var displayedCameras: [AtlasCityCamera] = []
        model?.onPresentedCamera = { displayedCameras.append($0) }
        try await until { model?.frame?.generation == 1 && model?.busy == false }
        precondition(displayedCameras.isEmpty, "an undisplayed candidate is not a refresh camera")
        try await acknowledge(model!, frame: 1)
        let old = model!.frame!, oldDisplay = model!.display
        precondition(displayedCameras == [old.camera])
        fixture.holdView(2); model?.move(.orbit(0.3, 0.1))
        try await until { fixture.counts.views == 2 }
        model?.resize(width: 400, height: 200, backing: 2)
        precondition(model?.canPick == false && model?.frame == nil && model?.display != oldDisplay)
        model?.presented(.init(view: old, frame: 2, display: oldDisplay))
        precondition(displayedCameras == [old.camera], "stale-size receipts cannot replace the retained camera")
        fixture.gate.signal()
        try await until { model?.frame?.width == 400 && model?.frame?.height == 200 && model?.busy == false }
        try await acknowledge(model!, frame: 3)
        precondition(displayedCameras.count == 2 && displayedCameras.last == model?.frame?.camera)
        model?.close()
        model?.presented(.init(view: old, frame: 4, display: oldDisplay)); model?.click(x: 50, y: 50)
        precondition(displayedCameras.count == 2, "a closed City cannot replace the refresh camera")
        precondition(model?.canPick == false && fixture.counts.picks == 0)
        try await finish(&model)
    }

    static func canceledOpenRetiresItsSceneAdmission() async throws {
        let fixture = CityFixture(); fixture.holdOpen()
        var model: AtlasCityModel? = make(fixture)
        try await until { fixture.counts.opens == 1 }
        model?.close(); model = nil
        precondition(AtlasCityLifetime.scenes.reservedSlots == 1,
            "an entered foreign call retains its preparation credit while the UI closes")
        fixture.gate.signal()
        try await until { fixture.counts.closes == 1 && AtlasCityLifetime.scenes.reservedSlots == 0 }
    }

    static func globalSceneAdmissionAndRetry() async throws {
        let firstFixture = CityFixture(), secondFixture = CityFixture(), thirdFixture = CityFixture()
        var first: AtlasCityModel? = make(firstFixture), second: AtlasCityModel? = make(secondFixture)
        try await until { first?.scene != nil && second?.scene != nil }
        var third: AtlasCityModel? = make(thirdFixture)
        precondition(third?.failed == true && thirdFixture.counts.creates == 0,
            "a third scene refuses before JSON, dictionaries or foreign scene allocation")
        first?.close(); first = nil; second?.close(); second = nil
        try await until { AtlasCityLifetime.scenes.reservedSlots == 0 }
        third?.retry()
        try await until { third?.scene != nil && third?.failed == false }
        try await finish(&third)
    }

    static func wrongOwnerCannotPublish() async throws {
        let fixture = CityFixture(wrongOwner: true)
        var model: AtlasCityModel? = make(fixture)
        try await until { model?.failed == true && fixture.counts.closes == 1 }
        precondition(model?.scene == nil && model?.canPick == false && fixture.counts.views == 0)
        try await finish(&model)
    }
}
