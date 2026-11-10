import Foundation

enum AtlasCityError: Error, Sendable {
    case unavailable, invalidResponse, closed, generationExhausted
    var message: String {
        switch self {
        case .unavailable: return "City is unavailable. Return to 2D or try preparing City again."
        case .invalidResponse: return "The City scene or displayed-frame receipt could not be verified. Return to 2D and reopen City."
        case .closed: return "This City scene is no longer current."
        case .generationExhausted: return "Reopen City to start a new view session."
        }
    }
}

/// Values copied from the accepted atlas layout, never a new layout or a grant
/// to read source. Native filenames remain in the caller's metadata authority.
struct AtlasCityParcel: Sendable {
    let id, bytes: UInt64
    let x, y, width, height: Double
    let known: Bool
    init(_ entry: AtlasProjectCatalog.Entry) {
        id = entry.id; bytes = entry.observedBytes
        x = entry.x; y = entry.y; width = entry.width; height = entry.height; known = true
    }
}

private struct AtlasCityNumber: Decodable {
    let value: UInt64
    init(from decoder: Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        guard let number = UInt64(text), String(number) == text else { throw AtlasCityError.invalidResponse }
        value = number
    }
}

struct AtlasCityScene: Sendable {
    struct Instance: Sendable {
        let id: UInt64
        let rect: SIMD4<Float>
        let height: Float
        let observedBytes: UInt64?
    }
    let owner, layout: UInt64
    let maximumHeight: Double
    let originX, originY: Double
    let unknownCount, cappedCount: UInt64
    let discoveryComplete: Bool
    let instances: [Instance]
    /// Every production scene carries its already reserved native lifetime.
    /// The payload behind it holds arrays/dictionaries without a lease cycle.
    let lifetime: AtlasNativeRetirement.Lease?
    static let maximumFiles = 20_000
    static let maximumReplyBytes = 16 * 1024 * 1024
    static let metricCap: UInt64 = 1_073_741_824

    static func decode(_ json: String?, parcels: [AtlasCityParcel], layout: UInt64,
                       complete: Bool, owner: UInt64? = nil,
                       lifetime: AtlasNativeRetirement.Lease? = nil) throws -> Self {
        let data = try AtlasCityWire.admit(json)
        let w = try JSONDecoder().decode(Wire.self, from: data)
        guard w.schema == "fcb.city/1", w.status == "ok", ["open", "info"].contains(w.command),
              w.owner.value > 0, owner == nil || w.owner.value == owner,
              w.layout.value == layout, layout > 0,
              w.metric == "capped-log-observed-bytes", w.metric_cap_bytes.value == metricCap,
              w.maximum_height.isFinite, w.maximum_height > 0,
              w.origin.count == 2, w.origin.allSatisfy(\.isFinite),
              !w.source_payload_read, w.discovery_complete == complete,
              parcels.count <= maximumFiles, w.instances.count == parcels.count,
              w.file_count.value == UInt64(parcels.count) else { throw AtlasCityError.invalidResponse }
        var byID: [UInt64: AtlasCityParcel] = [:]
        for parcel in parcels {
            guard parcel.id > 0, byID.updateValue(parcel, forKey: parcel.id) == nil,
                  [parcel.x, parcel.y, parcel.width, parcel.height].allSatisfy(\.isFinite),
                  parcel.width > 0, parcel.height > 0 else { throw AtlasCityError.invalidResponse }
        }
        var instances: [Instance] = []
        instances.reserveCapacity(parcels.count)
        var previous: UInt64 = 0, unknown: UInt64 = 0, capped: UInt64 = 0
        for instance in w.instances {
            let id = instance.file_id.value
            guard id > previous, let input = byID[id], instance.rect.count == 4,
                  instance.rect.allSatisfy(\.isFinite), instance.rect[2] > 0, instance.rect[3] > 0,
                  instance.height.isFinite, instance.height >= 0,
                  Double(instance.height) <= w.maximum_height * (1 + 0.000001),
                  instance.rect[0] == Float(input.x - w.origin[0]),
                  instance.rect[1] == Float(input.y - w.origin[1]),
                  instance.rect[2] == Float(input.width), instance.rect[3] == Float(input.height),
                  (instance.rect[0] + instance.rect[2]).isFinite,
                  (instance.rect[1] + instance.rect[3]).isFinite,
                  instance.rect[0] + instance.rect[2] > instance.rect[0],
                  instance.rect[1] + instance.rect[3] > instance.rect[1],
                  input.known ? instance.height > 0 : instance.height == 0,
                  instance.observed_bytes?.value == (input.known ? input.bytes : nil) else {
                throw AtlasCityError.invalidResponse
            }
            if !input.known { unknown += 1 }
            if input.known && input.bytes > metricCap { capped += 1 }
            previous = id
            instances.append(.init(id: id,
                rect: SIMD4(instance.rect[0], instance.rect[1], instance.rect[2], instance.rect[3]),
                height: instance.height, observedBytes: instance.observed_bytes?.value))
        }
        guard w.unknown_count.value == unknown, w.capped_count.value == capped else {
            throw AtlasCityError.invalidResponse
        }
        return Self(owner: w.owner.value, layout: layout, maximumHeight: w.maximum_height,
            originX: w.origin[0], originY: w.origin[1], unknownCount: unknown, cappedCount: capped,
            discoveryComplete: complete, instances: instances, lifetime: lifetime)
    }
    private struct Item: Decodable {
        let file_id: AtlasCityNumber
        let rect: [Float]
        let height: Float
        let observed_bytes: AtlasCityNumber?
    }
    private struct Wire: Decodable {
        let schema, status, command, metric: String
        let owner, layout, metric_cap_bytes, file_count, unknown_count, capped_count: AtlasCityNumber
        let maximum_height: Double
        let origin: [Double]
        let discovery_complete, source_payload_read: Bool
        let instances: [Item]
    }
}

struct AtlasCityCamera: Decodable, Sendable, Equatable {
    let center_x, center_y, scale, yaw, tilt: Double
    var valid: Bool {
        [center_x, center_y, scale, yaw, tilt].allSatisfy(\.isFinite) && scale > 0
    }
}

struct AtlasCityFrame: Sendable, Equatable {
    let owner, layout, generation: UInt64
    let width, height: Double
    /// Column-major Metal matrix; the shared engine is the sole projection owner.
    let matrix: [Float]
    let camera: AtlasCityCamera
    static func decode(_ json: String?, scene: AtlasCityScene, generation: UInt64,
                       width: Double, height: Double) throws -> Self {
        let w = try JSONDecoder().decode(Wire.self, from: AtlasCityWire.admit(json))
        guard w.schema == "fcb.city/1", w.status == "ok", w.command == "view",
              w.owner.value == scene.owner, w.layout.value == scene.layout,
              generation > 0, w.generation.value == generation,
              w.projection == "orthographic", !w.native_presented,
              width.isFinite, height.isFinite, width > 0, height > 0,
              w.width == width, w.height == height,
              w.matrix.count == 16, w.matrix.allSatisfy(\.isFinite),
              w.matrix.contains(where: { $0 != 0 }), w.camera.valid else {
            throw AtlasCityError.invalidResponse
        }
        return Self(owner: scene.owner, layout: scene.layout, generation: generation,
            width: width, height: height, matrix: w.matrix, camera: w.camera)
    }
    private struct Wire: Decodable {
        let schema, status, command, projection: String
        let owner, layout, generation: AtlasCityNumber
        let width, height: Double
        let matrix: [Float]
        let camera: AtlasCityCamera
        let native_presented: Bool
    }
}

struct AtlasCityReceipt: Sendable, Equatable {
    let view: AtlasCityFrame
    let frame, display: UInt64
    func acknowledge(_ json: String?) throws {
        let w = try JSONDecoder().decode(Present.self, from: AtlasCityWire.admit(json))
        guard w.schema == "fcb.city/1", w.status == "ok", w.command == "present",
              w.owner.value == view.owner, w.layout.value == view.layout,
              w.generation.value == view.generation, w.frame.value == frame,
              w.display.value == display, !w.native_presented,
              w.acknowledgment == "host-declared" else { throw AtlasCityError.invalidResponse }
    }
    func pick(_ json: String?) throws -> UInt64? {
        let w = try JSONDecoder().decode(Pick.self, from: AtlasCityWire.admit(json))
        guard w.schema == "fcb.city/1", w.status == "ok", w.command == "pick",
              w.owner.value == view.owner, w.layout.value == view.layout,
              w.generation.value == view.generation, w.frame.value == frame,
              w.display.value == display,
              w.picking == "frontmost-cuboid-not-source-location" else { throw AtlasCityError.invalidResponse }
        return w.file_id?.value
    }
    private struct Present: Decodable {
        let schema, status, command, acknowledgment: String
        let owner, layout, generation, frame, display: AtlasCityNumber
        let native_presented: Bool
    }
    private struct Pick: Decodable {
        let schema, status, command, picking: String
        let owner, layout, generation, frame, display: AtlasCityNumber
        let file_id: AtlasCityNumber?
    }
}

enum AtlasCityCommand: Sendable {
    case info
    case view(UInt64, Double, Double, AtlasCityCamera)
    case fit(UInt64, Double, Double)
    case pan(UInt64, UInt64, Double, Double)
    case zoom(UInt64, UInt64, Double, Double, Double)
    case orbit(UInt64, UInt64, Double, Double)
    case resize(UInt64, UInt64, Double, Double)
    case present(UInt64, UInt64, UInt64)
    case pick(UInt64, UInt64, Double, Double)
}

struct AtlasCityTransport: Sendable {
    let create: @Sendable () -> UInt64
    let open: @Sendable (UInt64, UInt64, [AtlasCityParcel], Bool) -> String?
    let request: @Sendable (UInt64, AtlasCityCommand) -> String?
    let close: @Sendable (UInt64) -> Bool
}

private enum AtlasCityWire {
    static func admit(_ json: String?) throws -> Data {
        guard let json, !json.isEmpty, json.utf8.count <= AtlasCityScene.maximumReplyBytes,
              let data = json.data(using: .utf8) else { throw AtlasCityError.invalidResponse }
        return data
    }
}

@_silgen_name("fcb_city_create") private func cityCreate() -> UInt64
@_silgen_name("fcb_city_open") private func cityOpen(_ handle: UInt64, _ layout: UInt64,
    _ ids: UnsafePointer<UInt64>?, _ rects: UnsafePointer<Double>?, _ bytes: UnsafePointer<UInt64>?,
    _ known: UnsafePointer<UInt8>?, _ count: UInt64, _ complete: UInt32) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_city_info") private func cityInfo(_ handle: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_city_view") private func cityView(_ handle: UInt64, _ generation: UInt64,
    _ width: Double, _ height: Double, _ centerX: Double, _ centerY: Double,
    _ scale: Double, _ yaw: Double, _ tilt: Double) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_city_fit") private func cityFit(_ handle: UInt64, _ generation: UInt64,
    _ width: Double, _ height: Double) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_city_pan") private func cityPan(_ handle: UInt64, _ generation: UInt64,
    _ base: UInt64, _ x: Double, _ y: Double) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_city_zoom") private func cityZoom(_ handle: UInt64, _ generation: UInt64,
    _ base: UInt64, _ x: Double, _ y: Double, _ factor: Double) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_city_orbit") private func cityOrbit(_ handle: UInt64, _ generation: UInt64,
    _ base: UInt64, _ yaw: Double, _ tilt: Double) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_city_resize") private func cityResize(_ handle: UInt64, _ generation: UInt64,
    _ base: UInt64, _ width: Double, _ height: Double) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_city_present") private func cityPresent(_ handle: UInt64, _ generation: UInt64,
    _ frame: UInt64, _ display: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_city_pick") private func cityPick(_ handle: UInt64, _ frame: UInt64,
    _ display: UInt64, _ x: Double, _ y: Double) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_city_close") private func cityClose(_ handle: UInt64) -> UInt32
@_silgen_name("fcb_free_string") private func cityFree(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasCityTransport {
    static let native = Self(create: { cityCreate() }, open: { handle, layout, parcels, complete in
        guard parcels.count <= AtlasCityScene.maximumFiles else { return nil }
        let ids = parcels.map(\.id), bytes = parcels.map(\.bytes)
        let known: [UInt8] = parcels.map { $0.known ? 1 : 0 }
        var rects: [Double] = []; rects.reserveCapacity(parcels.count * 4)
        for parcel in parcels { rects.append(contentsOf: [parcel.x, parcel.y, parcel.width, parcel.height]) }
        return ids.withUnsafeBufferPointer { ids in
            rects.withUnsafeBufferPointer { rects in
                bytes.withUnsafeBufferPointer { bytes in
                    known.withUnsafeBufferPointer { known in
                        take(cityOpen(handle, layout, ids.baseAddress, rects.baseAddress,
                            bytes.baseAddress, known.baseAddress, UInt64(parcels.count), complete ? 1 : 0))
                    }
                }
            }
        }
    }, request: { handle, command in
        switch command {
        case .info: return take(cityInfo(handle))
        case .view(let g, let w, let h, let c):
            return take(cityView(handle, g, w, h, c.center_x, c.center_y, c.scale, c.yaw, c.tilt))
        case .fit(let g, let w, let h): return take(cityFit(handle, g, w, h))
        case .pan(let g, let b, let x, let y): return take(cityPan(handle, g, b, x, y))
        case .zoom(let g, let b, let x, let y, let f): return take(cityZoom(handle, g, b, x, y, f))
        case .orbit(let g, let b, let y, let t): return take(cityOrbit(handle, g, b, y, t))
        case .resize(let g, let b, let w, let h): return take(cityResize(handle, g, b, w, h))
        case .present(let g, let f, let d): return take(cityPresent(handle, g, f, d))
        case .pick(let f, let d, let x, let y): return take(cityPick(handle, f, d, x, y))
        }
    }, close: { cityClose($0) == 1 })

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { cityFree(pointer) }
        var count = 0
        while count <= AtlasCityScene.maximumReplyBytes && pointer[count] != 0 { count += 1 }
        guard count > 0, count <= AtlasCityScene.maximumReplyBytes else { return nil }
        return String(data: Data(bytes: pointer, count: count), encoding: .utf8)
    }
}
