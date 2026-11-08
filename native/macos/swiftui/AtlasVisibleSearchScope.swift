import Foundation

/// An explicit snapshot of file parcels in one accepted atlas view. This is
/// search membership, independent of source-preview residency and text LOD.
/// The search service still captures and verifies the requested files itself.
struct AtlasVisibleSearchScope: Sendable {
    enum Failure: Error, Sendable {
        case invalidView, invalidMetadata, tooManyFiles(Int), tooManyPathBytes, pathTooLong, canceled

        var message: String {
            switch self {
            case .invalidView:
                return "Show the text atlas and wait for a displayed view before choosing its files."
            case .invalidMetadata:
                return "The atlas view could not be verified. Refresh the atlas before choosing a visible search scope."
            case .tooManyFiles(let maximum):
                return "This view contains more than \(maximum) files. Zoom in, then use the current view again."
            case .tooManyPathBytes:
                return "This view contains too much filename data for one search. Zoom in, then use the current view again."
            case .pathTooLong:
                return "A filename in this view exceeds the visible-search path limit. Choose a smaller view."
            case .canceled:
                return "Choosing visible files stopped."
            }
        }
    }

    /// Foundation-only geometry keeps membership tests independent of AppKit,
    /// drawing, source loading, and any particular renderer.
    struct Viewport: Equatable, Sendable {
        let x, y, width, height: Double

        init(x: Double, y: Double, width: Double, height: Double) throws {
            guard [x, y, width, height].allSatisfy(\.isFinite),
                  abs(x) <= 1e12, abs(y) <= 1e12,
                  width > 0, height > 0, width <= 1e12, height <= 1e12 else {
                throw Failure.invalidView
            }
            self.x = x; self.y = y; self.width = width; self.height = height
        }

        fileprivate func intersects(_ entry: AtlasProjectCatalog.Entry) -> Bool {
            // Touching an edge contributes no visible area. Small parcels and
            // parcels without resident text count exactly like large parcels.
            max(x, entry.x) < min(x + width, entry.x + entry.width)
                && max(y, entry.y) < min(y + height, entry.y + entry.height)
        }
    }

    struct View: Sendable {
        let id: UUID
        let root: String
        let project, layout: UUID
        let viewport: Viewport

        init(root: String, project: UUID, layout: UUID, viewport: Viewport, id: UUID = UUID()) throws {
            guard root.hasPrefix("/"), root.utf8.count <= 16_384, !root.utf8.contains(0) else {
                throw Failure.invalidView
            }
            self.id = id; self.root = root; self.project = project; self.layout = layout; self.viewport = viewport
        }

        func belongs(to root: String, project: UUID, layout: UUID) -> Bool {
            self.project == project && self.layout == layout && self.root.utf8.elementsEqual(root.utf8)
        }

        func represents(root: String, project: UUID, layout: UUID, viewport: Viewport) -> Bool {
            belongs(to: root, project: project, layout: layout) && self.viewport == viewport
        }
    }

    let id: UUID
    let view: View
    let paths: [[UInt8]]
    let encodedPathBytes: Int
    var count: Int { paths.count }

    /// Run on the existing project worker. The input metadata has a fixed
    /// admission limit; the output is refused in full if either scope limit is
    /// reached. No omitted parcel can become a successful no-match result.
    static func capture(view: View, entries: [AtlasProjectCatalog.Entry],
        maximumFiles: Int, maximumWireBytes: Int, maximumPathBytes: Int,
        id: UUID = UUID(), canceled: @Sendable () -> Bool = { false }) throws -> Self {
        guard entries.count <= AtlasProjectCatalog.maximumFiles, maximumFiles > 0,
              maximumFiles <= AtlasProjectCatalog.maximumFiles, maximumWireBytes > 0,
              maximumWireBytes <= 1024 * 1024, maximumPathBytes > 0, maximumPathBytes <= 16_384 else {
            throw Failure.invalidMetadata
        }
        guard !canceled() else { throw Failure.canceled }
        var paths: [[UInt8]] = [], seen = Set<[UInt8]>(), encodedBytes = 0
        paths.reserveCapacity(min(entries.count, maximumFiles))
        seen.reserveCapacity(min(entries.count, maximumFiles))
        for entry in entries {
            guard !canceled() else { throw Failure.canceled }
            guard entry.id > 0, [entry.x, entry.y, entry.width, entry.height].allSatisfy(\.isFinite),
                  abs(entry.x) <= 1e12, abs(entry.y) <= 1e12,
                  entry.width > 0, entry.height > 0, entry.width <= 1e12, entry.height <= 1e12 else {
                throw Failure.invalidMetadata
            }
            guard view.viewport.intersects(entry) else { continue }
            let raw = entry.rawPath
            guard raw.count <= maximumPathBytes else { throw Failure.pathTooLong }
            guard !raw.isEmpty, !raw.contains(0),
                  !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                      $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
                  }) else { throw Failure.invalidMetadata }
            guard !seen.contains(raw) else { continue }
            guard paths.count < maximumFiles else { throw Failure.tooManyFiles(maximumFiles) }
            let cost = raw.count * 2 + (paths.isEmpty ? 0 : 1)
            guard cost <= maximumWireBytes - encodedBytes else { throw Failure.tooManyPathBytes }
            seen.insert(raw); paths.append(raw); encodedBytes += cost
        }
        guard !canceled() else { throw Failure.canceled }
        return Self(id: id, view: view, paths: paths, encodedPathBytes: encodedBytes)
    }
}
