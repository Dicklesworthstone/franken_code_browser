import Foundation
import Observation
import Dispatch

enum AtlasCityLifetime {
    // At most one current and one preparing/retiring City scene across all
    // windows. This covers metadata, open JSON/decoding overlap, dictionaries,
    // retained instances, upload scratch and the small fixed Metal pipeline.
    static let preparationBytes = 128 * 1024 * 1024
    static let scenes = AtlasNativeRetirement(maximumSlots: 2, maximumBytes: 2 * preparationBytes)
    // Each target admits depth plus all three potential color drawables. Old
    // targets remain charged through their command flights and final release.
    static let targets = AtlasNativeRetirement(maximumSlots: 4, maximumBytes: 512 * 1024 * 1024)
}

struct AtlasCityPrepared: Sendable {
    let scene: AtlasCityScene
    let files: [UInt64: AtlasProjectCatalog.Entry]
    let indices: [[UInt8]: UInt32]
}

/// Written only by the preparation worker, then immutable. Publishing this
/// holder before work also retires partial/failing preparation off the UI.
private final class AtlasCityStorage: @unchecked Sendable {
    let entries: [AtlasProjectCatalog.Entry]
    var instances: [AtlasCityScene.Instance] = []
    var files: [UInt64: AtlasProjectCatalog.Entry] = [:]
    var indices: [[UInt8]: UInt32] = [:]
    init(entries: [AtlasProjectCatalog.Entry]) { self.entries = entries }
}

/// The only owner of the foreign session. Every bridge call and the final
/// release runs on this serial native-host worker, including lost-open cleanup.
final class AtlasCityWorker: @unchecked Sendable {
    private static let queue = DispatchQueue(label: "dev.frankencode.browser.city", qos: .userInitiated)
    private let transport: AtlasCityTransport
    private var handle: UInt64 = 0
    private var closed = false
    init(transport: AtlasCityTransport) { self.transport = transport }

    func open(entries: [AtlasProjectCatalog.Entry], complete: Bool,
              lifetime: AtlasNativeRetirement.Lease) async throws -> AtlasCityPrepared {
        try await perform {
            guard self.handle == 0, !self.closed else { throw AtlasCityError.closed }
            guard !entries.isEmpty, entries.count <= AtlasCityScene.maximumFiles else { throw AtlasCityError.unavailable }
            let storage = AtlasCityStorage(entries: entries)
            guard lifetime.publish(storage: storage, bytes: AtlasCityLifetime.preparationBytes) else {
                throw AtlasCityError.unavailable
            }
            let handle = self.transport.create()
            guard handle > 0 else { throw AtlasCityError.unavailable }
            self.handle = handle
            do {
                let parcels = entries.map(AtlasCityParcel.init)
                let scene: AtlasCityScene
                do {
                    scene = try AtlasCityScene.decode(self.transport.open(handle, 1, parcels, complete),
                        parcels: parcels, layout: 1, complete: complete, owner: handle, lifetime: lifetime)
                } catch {
                    // An accepted open may outlive its reply. Re-read the same
                    // retained scene; never open another scene under that handle.
                    scene = try AtlasCityScene.decode(self.transport.request(handle, .info),
                        parcels: parcels, layout: 1, complete: complete, owner: handle, lifetime: lifetime)
                }
                storage.instances = scene.instances
                storage.files.reserveCapacity(entries.count); storage.indices.reserveCapacity(entries.count)
                for entry in entries {
                    guard storage.files.updateValue(entry, forKey: entry.id) == nil else { throw AtlasCityError.invalidResponse }
                }
                for (index, instance) in scene.instances.enumerated() {
                    guard let entry = storage.files[instance.id], storage.indices.updateValue(UInt32(index), forKey: entry.rawPath) == nil else {
                        throw AtlasCityError.invalidResponse
                    }
                }
                return AtlasCityPrepared(scene: scene, files: storage.files, indices: storage.indices)
            } catch {
                _ = self.transport.close(handle); self.handle = 0
                throw error
            }
        }
    }

    func view(_ command: AtlasCityCommand, scene: AtlasCityScene, generation: UInt64,
              width: Double, height: Double) async throws -> AtlasCityFrame {
        try await perform {
            try self.requireOpen()
            return try AtlasCityFrame.decode(self.transport.request(self.handle, command),
                scene: scene, generation: generation, width: width, height: height)
        }
    }
    func present(_ receipt: AtlasCityReceipt) async throws {
        try await perform {
            try self.requireOpen()
            let command = AtlasCityCommand.present(receipt.view.generation, receipt.frame, receipt.display)
            do { try receipt.acknowledge(self.transport.request(self.handle, command)) }
            catch {
                // A repeated receipt is idempotent. This bounded retry resolves
                // an accepted declaration whose first reply was lost.
                try receipt.acknowledge(self.transport.request(self.handle, command))
            }
        }
    }
    func pick(_ receipt: AtlasCityReceipt, x: Double, y: Double) async throws -> UInt64? {
        try await perform {
            try self.requireOpen()
            return try receipt.pick(self.transport.request(self.handle, .pick(receipt.frame, receipt.display, x, y)))
        }
    }
    func close() {
        Self.queue.async { [self] in
            closed = true
            if handle != 0 { _ = transport.close(handle); handle = 0 }
        }
    }
    private func requireOpen() throws {
        guard !closed, handle > 0 else { throw AtlasCityError.closed }
    }
    private func perform<Value: Sendable>(_ body: @escaping @Sendable () throws -> Value) async throws -> Value {
        try await withCheckedThrowingContinuation { continuation in
            Self.queue.async { continuation.resume(with: Result { try body() }) }
        }
    }
    deinit {
        let handle = handle, transport = transport
        if handle != 0 { Self.queue.async { _ = transport.close(handle) } }
    }
}

struct AtlasCitySeed: Sendable {
    let scale, offsetX, offsetY: Double
    let retainedCamera: AtlasCityCamera?
    init(scale: Double, offsetX: Double, offsetY: Double, retainedCamera: AtlasCityCamera? = nil) {
        self.scale = scale; self.offsetX = offsetX; self.offsetY = offsetY
        self.retainedCamera = retainedCamera
    }
    func camera(width: Double, height: Double) -> AtlasCityCamera? {
        if let retainedCamera { return retainedCamera.valid ? retainedCamera : nil }
        guard [scale, offsetX, offsetY].allSatisfy(\.isFinite), scale > 0 else { return nil }
        let camera = AtlasCityCamera(center_x: (width / 2 - offsetX) / scale,
            center_y: (height / 2 - offsetY) / scale, scale: scale, yaw: 0, tilt: 0.7)
        return camera.valid ? camera : nil
    }
}

enum AtlasCityMotion: Sendable, Equatable {
    case initial, fit, pan(Double, Double), zoom(Double, Double, Double), orbit(Double, Double), resize
    /// There is one newest waiting motion. Repeated events of the same gesture
    /// accumulate in fixed storage; a different gesture replaces waiting input.
    func coalescing(_ next: Self) -> Self {
        func clamp(_ n: Double, _ limit: Double) -> Double { max(-limit, min(limit, n)) }
        switch (self, next) {
        case (.pan(let x, let y), .pan(let dx, let dy)):
            return .pan(clamp(x + dx, 16_384), clamp(y + dy, 16_384))
        case (.orbit(let y, let t), .orbit(let dy, let dt)):
            return .orbit(clamp(y + dy, 2 * .pi), clamp(t + dt, .pi))
        case (.zoom(let x, let y, let f), .zoom(let nx, let ny, let nf)) where x == nx && y == ny:
            return .zoom(x, y, max(1.0 / 16, min(16, f * nf)))
        default: return next
        }
    }
}

/// One active worker operation, one coalesced motion, one click, and one newest
/// displayed receipt. A prepared camera is not clickable until Metal reports
/// actual display and the host acknowledges exactly that frame/display pair.
@MainActor @Observable final class AtlasCityModel {
    let revision: UUID
    private(set) var scene: AtlasCityScene?
    private(set) var frame: AtlasCityFrame?
    private(set) var display: UInt64 = 0
    private(set) var selectedIndex: UInt32 = .max
    private(set) var notice = "Preparing City from the retained atlas footprints…"
    private(set) var failed = false
    private(set) var busy = false
    private(set) var closed = false
    private var entries: [AtlasProjectCatalog.Entry]
    private let complete: Bool
    private let seed: AtlasCitySeed?
    private let transport: AtlasCityTransport
    private var worker: AtlasCityWorker
    private var workerStarted = false
    private var files: [UInt64: AtlasProjectCatalog.Entry] = [:]
    private var indices: [[UInt8]: UInt32] = [:]
    private var selectedPath: [UInt8]?
    private var operation = UUID()
    private var task: Task<Void, Never>?
    private var generation: UInt64 = 0
    private var width = 0.0, height = 0.0, backing = 0.0
    private var base: AtlasCityFrame?
    private var awaitingPresentation: UInt64?
    private var actual: AtlasCityReceipt?
    private var accepted: AtlasCityReceipt?
    private var pendingReceipt: AtlasCityReceipt?
    private var pendingMotion: AtlasCityMotion?
    private struct Click { let receipt: AtlasCityReceipt; let x, y: Double }
    private var pendingClick: Click?
    var onSelect: ((AtlasProjectCatalog.Entry) -> Void)?
    var onPresentedCamera: ((AtlasCityCamera) -> Void)?

    init(revision: UUID, entries: [AtlasProjectCatalog.Entry], complete: Bool, seed: AtlasCitySeed? = nil,
         transport: AtlasCityTransport = .native) {
        self.revision = revision; self.entries = entries; self.complete = complete; self.transport = transport
        self.seed = seed
        worker = AtlasCityWorker(transport: transport)
    }
    var ready: Bool { scene != nil && !failed && !closed }
    var canPick: Bool { ready && actual != nil && accepted == actual && actual?.view == frame }
    var selectedDescription: String? {
        guard let scene, Int(selectedIndex) < scene.instances.count,
              let entry = files[scene.instances[Int(selectedIndex)].id] else { return nil }
        return entry.displayPath
    }

    func start() {
        guard task == nil, scene == nil, !closed else { return }
        guard let lifetime = AtlasCityLifetime.scenes.reserve(bytes: AtlasCityLifetime.preparationBytes) else {
            renderingFailed("City is finishing a previous scene or another window is using its memory allowance. Retry shortly, or return to 2D.")
            return
        }
        let ticket = operation, worker = worker, entries = entries, complete = complete
        busy = true; failed = false; workerStarted = true
        task = Task { [weak self] in
            do {
                let prepared = try await worker.open(entries: entries, complete: complete, lifetime: lifetime)
                guard let self, self.operation == ticket, !self.closed else { return }
                self.scene = prepared.scene; self.files = prepared.files; self.indices = prepared.indices
                self.updateSelectedIndex(); self.pendingMotion = .initial
                self.notice = "Preparing the first City view…"
                self.finishOperation()
            } catch {
                guard let self, self.operation == ticket, !self.closed else { return }
                self.fail(error)
            }
        }
    }

    func resize(width: Double, height: Double, backing: Double) {
        guard !closed, width.isFinite, height.isFinite, backing.isFinite,
              width > 1, height > 1, width <= 32_768, height <= 32_768, backing >= 1, backing <= 8,
              width != self.width || height != self.height || backing != self.backing else { return }
        guard display < .max else { fail(AtlasCityError.generationExhausted); return }
        self.width = width; self.height = height; self.backing = backing; display += 1
        // Resizing changes the pixel coordinate domain immediately. Old pixels
        // may remain visible during preparation, but can no longer activate.
        actual = nil; accepted = nil; pendingReceipt = nil; pendingClick = nil
        frame = nil; awaitingPresentation = nil; pendingMotion = base == nil ? .initial : .resize
        pump()
    }
    func move(_ motion: AtlasCityMotion) {
        guard ready else { return }
        switch motion {
        case .pan(let x, let y), .orbit(let x, let y):
            guard x.isFinite, y.isFinite else { return }
        case .zoom(let x, let y, let factor):
            guard x.isFinite, y.isFinite, factor.isFinite, factor > 0 else { return }
        case .initial, .fit, .resize: break
        }
        pendingMotion = pendingMotion.map { $0.coalescing(motion) } ?? motion
        pump()
    }
    func selectPath(_ path: String?) {
        selectedPath = path.map { Array($0.utf8) }
        updateSelectedIndex()
    }
    func readAdjacent(_ direction: Int) {
        guard ready, let scene, !scene.instances.isEmpty else { return }
        let count = scene.instances.count
        let current = Int(selectedIndex) < count ? Int(selectedIndex) : (direction < 0 ? 0 : count - 1)
        var next = current
        for _ in 0..<count {
            next = (next + (direction < 0 ? count - 1 : 1)) % count
            guard let file = files[scene.instances[next].id], file.sourcePath != nil else { continue }
            // The shared source opener publishes the selection. A failed or
            // stale activation must not manufacture a different City selection.
            onSelect?(file); return
        }
    }
    func click(x: Double, y: Double) {
        guard canPick, let actual, actual.display == display,
              x.isFinite, y.isFinite, x >= 0, y >= 0, x < width, y < height else { return }
        pendingClick = Click(receipt: actual, x: x, y: y)
        pump()
    }

    func presented(_ receipt: AtlasCityReceipt) {
        guard ready, let scene, receipt.view.owner == scene.owner, receipt.view.layout == scene.layout,
              receipt.display == display, receipt.view == frame,
              receipt.view.width == width, receipt.view.height == height,
              receipt.frame > 0, actual.map({ receipt.frame > $0.frame }) ?? true else { return }
        // The Metal surface calls only after a positive actual presentedTime.
        actual = receipt; accepted = nil; pendingReceipt = receipt
        onPresentedCamera?(receipt.view.camera)
        if pendingClick?.receipt != receipt { pendingClick = nil }
        pump()
    }
    func renderingFailed(_ message: String) {
        guard !closed else { return }
        failed = true; notice = message; frame = nil; actual = nil; accepted = nil
        pendingMotion = nil; pendingClick = nil; pendingReceipt = nil; awaitingPresentation = nil
    }
    func retry() {
        guard !closed else { return }
        operation = UUID(); task?.cancel(); task = nil
        if workerStarted { worker.close() }
        worker = AtlasCityWorker(transport: transport)
        workerStarted = false
        scene = nil; frame = nil; base = nil; files = [:]; indices = [:]
        actual = nil; accepted = nil; pendingReceipt = nil; pendingClick = nil; pendingMotion = nil
        awaitingPresentation = nil; generation = 0; selectedIndex = .max; busy = false
        notice = "Preparing City from the retained atlas footprints…"; failed = false
        start()
    }
    func close() {
        guard !closed else { return }
        closed = true; operation = UUID(); task?.cancel(); task = nil
        if workerStarted { worker.close(); workerStarted = false }
        actual = nil; accepted = nil; pendingReceipt = nil; pendingClick = nil; pendingMotion = nil
        awaitingPresentation = nil; frame = nil; busy = false; onSelect = nil; onPresentedCamera = nil
        scene = nil; base = nil; files = [:]; indices = [:]
        entries = []
    }

    private func updateSelectedIndex() {
        selectedIndex = selectedPath.flatMap { indices[$0] } ?? .max
    }
    private func finishOperation() {
        task = nil; busy = false; pump()
    }
    private func fail(_ error: Error) {
        task = nil; busy = false
        renderingFailed((error as? AtlasCityError)?.message ?? AtlasCityError.invalidResponse.message)
    }
    private func pump() {
        guard ready, task == nil, let scene, width > 1, height > 1 else { return }
        let worker = worker, ticket = operation
        if let receipt = pendingReceipt {
            pendingReceipt = nil; busy = true
            task = Task { [weak self] in
                do {
                    try await worker.present(receipt)
                    guard let self, self.operation == ticket, !self.closed else { return }
                    if self.actual == receipt && self.display == receipt.display && self.frame == receipt.view {
                        self.accepted = receipt; self.notice = ""
                        if self.awaitingPresentation == receipt.view.generation { self.awaitingPresentation = nil }
                    }
                    self.finishOperation()
                } catch {
                    guard let self, self.operation == ticket, !self.closed else { return }
                    self.fail(error)
                }
            }
            return
        }
        if let click = pendingClick {
            pendingClick = nil
            guard click.receipt == actual, click.receipt == accepted else { pump(); return }
            busy = true
            task = Task { [weak self] in
                do {
                    let id = try await worker.pick(click.receipt, x: click.x, y: click.y)
                    guard let self, self.operation == ticket, !self.closed else { return }
                    if self.actual == click.receipt && self.accepted == click.receipt && self.frame == click.receipt.view, let id {
                        guard let file = self.files[id] else { throw AtlasCityError.invalidResponse }
                        self.onSelect?(file)
                    }
                    self.finishOperation()
                } catch {
                    guard let self, self.operation == ticket, !self.closed else { return }
                    // A later displayed receipt may retire a pending click.
                    if self.actual != click.receipt { self.finishOperation() }
                    else { self.fail(error) }
                }
            }
            return
        }
        guard awaitingPresentation == nil, let motion = pendingMotion else { return }
        pendingMotion = nil
        guard generation < .max else { fail(AtlasCityError.generationExhausted); return }
        generation += 1
        let generation = generation, width = width, height = height, display = display
        let command: AtlasCityCommand
        if let base {
            if base.width != width || base.height != height {
                command = .resize(generation, base.generation, width, height)
                if motion != .resize { pendingMotion = motion }
            } else {
            switch motion {
            case .initial, .fit: command = .fit(generation, width, height)
            case .resize: command = .resize(generation, base.generation, width, height)
            case .pan(let x, let y): command = .pan(generation, base.generation, x, y)
            case .zoom(let x, let y, let factor): command = .zoom(generation, base.generation, x, y, factor)
            case .orbit(let yaw, let tilt): command = .orbit(generation, base.generation, yaw, tilt)
            }
            }
        } else if motion == .initial, let camera = seed?.camera(width: width, height: height) {
            command = .view(generation, width, height, camera)
        } else { command = .fit(generation, width, height) }
        busy = true
        task = Task { [weak self] in
            do {
                let candidate = try await worker.view(command, scene: scene, generation: generation, width: width, height: height)
                guard let self, self.operation == ticket, !self.closed else { return }
                self.base = candidate
                if self.display == display && self.width == width && self.height == height {
                    // An input request can be encoded before its actual display
                    // callback reaches main. Fence the old camera immediately.
                    self.actual = nil; self.accepted = nil; self.pendingClick = nil; self.pendingReceipt = nil
                    self.frame = candidate; self.awaitingPresentation = candidate.generation
                } else { self.pendingMotion = .resize }
                self.finishOperation()
            } catch {
                guard let self, self.operation == ticket, !self.closed else { return }
                self.fail(error)
            }
        }
    }
}
