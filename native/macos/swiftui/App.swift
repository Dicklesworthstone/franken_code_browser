// FrankenCodeBrowser — SwiftUI application shell over the real fcb engine.
//
// The main surface is the ATLAS: fcb-map's partition layout positions
// every file in the workspace. Every admitted text file displays its actual
// captured glyphs at overview and detail scales, independent of selection.
// Whole-source admission remains bounded; errors are visible. All data flows
// through the fcb-bridge staticlib into the same engine the `fcb` CLI
// uses.

import SwiftUI
import UniformTypeIdentifiers
import AppKit
import CoreText

// MARK: - C ABI (fcb-bridge staticlib)

@_silgen_name("fcb_search_workspace")
func fcb_search_workspace(_ root: UnsafePointer<CChar>?, _ query: UnsafePointer<CChar>?)
    -> UnsafeMutablePointer<CChar>?

@_silgen_name("fcb_read_file")
func fcb_read_file(_ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?

@_silgen_name("fcb_source_document")
func fcb_source_document(_ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?

@_silgen_name("fcb_text_tile_positions")
func fcb_text_tile_positions(_ heights: UnsafePointer<Double>?, _ count: UInt64,
    _ columns: UInt64, _ width: Double, _ gap: Double) -> UnsafeMutablePointer<CChar>?

@_silgen_name("fcb_atlas_layout")
func fcb_atlas_layout(_ root: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?

@_silgen_name("fcb_atlas_plan")
func fcb_atlas_plan(_ root: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?

@_silgen_name("fcb_free_string")
func fcb_free_string(_ pointer: UnsafeMutablePointer<CChar>?)

private func bridgeCall(_ body: () -> UnsafeMutablePointer<CChar>?) -> String {
    guard let pointer = body() else { return "" }
    defer { fcb_free_string(pointer) }
    return String(cString: pointer)
}

// MARK: - Models

struct AtlasFile: Identifiable, Hashable, Sendable {
    let path: String
    let x: Double
    let y: Double
    let w: Double
    let h: Double
    let bytes: Int
    let lineCount: Int
    let sourceLineCount: Int?
    let profileState: String
    /// Packed per-line profile: [length_frac, class] × lineCount.
    let profile: [UInt8]
    var id: String { path }
    /// List identity uses Unix bytes, not Swift canonical-equivalent equality.
    var rawPathKey: [UInt8] { Array(path.utf8) }

    var fileName: String { (path as NSString).lastPathComponent }
    var previewCoverage: String {
        switch profileState {
        case "complete": return "All lines previewed"
        case "row-limit": return "First \(lineCount) lines only"
        case "file-byte-limit": return "File exceeds preview size limit"
        case "global-byte-limit": return "Project preview budget reached"
        case "unsupported-text": return "Text format unavailable"
        case "unavailable-or-changed": return "File changed or unavailable"
        case "disabled": return "Preview disabled"
        default: return "Unknown"
        }
    }

    /// Average tile color from the line profile — one line, one hue.
    let avgColor: Color

    static func averageColor(profile: [UInt8]) -> Color {
        guard profile.count >= 2 else { return Color(red: 0.16, green: 0.30, blue: 0.28) }
        var r = 0.0, g = 0.0, b = 0.0, weight = 0.0
        var index = 0
        while index + 1 < profile.count {
            let length = Double(profile[index]) / 255.0
            let role = profile[index + 1]
            let color = Self.roleBaseColor(role)
            r += color.0 * length
            g += color.1 * length
            b += color.2 * length
            weight += length
            index += 2
        }
        guard weight > 0 else { return Color(red: 0.16, green: 0.30, blue: 0.28) }
        return Color(red: r / weight, green: g / weight, blue: b / weight)
    }

    static func roleBaseColor(_ role: UInt8) -> (Double, Double, Double) {
        switch role {
        case 1: return (0.36, 0.35, 0.30) // comments
        case 2: return (0.92, 0.78, 0.28) // strings
        case 3: return (0.88, 0.28, 0.48) // keywords
        default: return (0.26, 0.66, 0.60) // code
        }
    }

    static func roleColor(_ role: UInt8) -> Color {
        let base = roleBaseColor(role)
        return Color(red: base.0, green: base.1, blue: base.2)
    }
}


enum Engine {
    static let defaultRoot = ""
    private static let recentsKey = "fcb.recentRoots"
    static let maxRecents = 6

    static var recents: [String] {
#if FCB_APP_STORE
        AppStoreRootAccess.recents
#else
        UserDefaults.standard.stringArray(forKey: recentsKey) ?? []
#endif
    }

#if !FCB_APP_STORE
    static func remember(root: String) {
        var roots = recents.filter { $0 != root }
        roots.insert(root, at: 0)
        UserDefaults.standard.set(Array(roots.prefix(maxRecents)), forKey: recentsKey)
    }
#endif

    static func plan(root: String) -> String {
        bridgeCall { fcb_atlas_plan(root) }
    }

    static func atlas(root: String) -> [AtlasFile] {
        let json = bridgeCall { fcb_atlas_layout(root) }
        guard let data = json.data(using: .utf8),
            let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let files = object["files"] as? [[String: Any]]
        else { return [] }
        return files.compactMap { file in
            guard let path = file["path"] as? String else { return nil }
            let number = { (key: String) in (file[key] as? NSNumber)?.doubleValue ?? 0 }
            let lineCount = (file["n"] as? NSNumber)?.intValue ?? 0
            let profile: [UInt8]
            if let tex = file["tex"] as? String, let decoded = Data(base64Encoded: tex) {
                profile = [UInt8](decoded)
            } else {
                profile = []
            }
            return AtlasFile(
                path: path, x: number("x"), y: number("y"), w: number("w"), h: number("h"),
                bytes: (file["bytes"] as? NSNumber)?.intValue ?? 0,
                lineCount: lineCount,
                sourceLineCount: (file["source_lines"] as? String).flatMap(Int.init),
                profileState: file["profile_state"] as? String ?? "unknown",
                profile: profile, avgColor: AtlasFile.averageColor(profile: profile)
            )
        }
    }

    static func sourceCapture(root: String, path: String) -> AtlasHighlightCapture? {
        let full = (root as NSString).appendingPathComponent(path)
        guard let pointer = fcb_source_document(full) else { return nil }
        defer { fcb_free_string(pointer) }
        guard let data = String(cString: pointer).data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(AtlasHighlightCapture.self, from: data)
    }

    static func placeTextTiles(_ tiles: [AtlasTextTile]) -> CGRect? {
        guard !tiles.isEmpty, tiles.count <= 65536 else { return nil }
        guard let bounds = AtlasParcelLayout.place(tiles) else { return nil }
        let pixelsPerTile = max(1, 16 * 1024 * 1024 / tiles.count)
        var remainingPixels = 16 * 1024 * 1024
        for (index, tile) in tiles.enumerated() {
            let available = max(1, remainingPixels - (tiles.count - index - 1))
            if tile.raster == nil {
                tile.prepareRaster(pixelBudget: min(pixelsPerTile, available))
            }
            remainingPixels = max(0, remainingPixels - (tile.raster.map { $0.width * $0.height } ?? 0))
        }
        return bounds
    }

    static func search(root: String, query: String) throws -> AtlasSearchReport {
        guard let pointer = fcb_search_workspace(root, query) else { throw AtlasSearchError.unavailable }
        defer { fcb_free_string(pointer) }
        return try AtlasSearchReport.decode(String(cString: pointer))
    }

    static func read(root: String, path: String) -> String? {
        let full = root.hasSuffix("/") ? root + path : root + "/" + path
        guard let pointer = fcb_read_file(full) else { return nil }
        defer { fcb_free_string(pointer) }
        return String(cString: pointer)
    }
}

// The camera is shared with the standalone production regression tests.
extension AtlasCamera {
    func screenRect(_ file: AtlasFile) -> CGRect {
        CGRect(x: file.x * scale + offsetX, y: file.y * scale + offsetY,
               width: file.w * scale, height: file.h * scale)
    }
    func file(at point: CGPoint, files: [AtlasFile]) -> AtlasFile? {
        files.last { screenRect($0).contains(point) }
    }
}

// MARK: - Content

private struct AtlasFrozenViewportSearch: Sendable {
    let snapshot: AtlasVisibleSearchScope
    let pathScope: AtlasSearchPathScope
    let catalogScope: AtlasSearchCatalogScope
}

/// A complete atlas replacement is staged on the bounded worker before any
/// displayed ownership changes. No metadata scan, filtering, packing, spatial
/// indexing or file-presentation construction is needed at publication.
private struct AtlasPreparedProjectRefresh: Sendable {
    let catalog: AtlasProjectCatalog
    let visibleCatalog: [AtlasProjectCatalog.Entry]
    let reconciliation: AtlasRefreshLayout.Result
    let layout: AtlasViewportLayout
    let files: [AtlasFile]
    let bounds: CGRect
    let addedBounds: CGRect?
    let retainingPreviousMembership: Bool

    static func prepare(root: String, directory: AtlasDirectoryFocus, discoveryPolicy: AtlasDiscoveryPolicy,
        previous: [AtlasProjectCatalog.Entry], previousBounds: CGRect, retainingPreviousMembership: Bool,
        scope: AtlasSearchFileScope, includesUnsupportedNames: Bool,
        cancellation: AtlasSearchCancellation) throws -> Self {
        let catalog = try AtlasProjectWorker.catalogReport(root: root, directory: directory, discoveryPolicy: discoveryPolicy,
            cancellation: cancellation)
        let visible = catalog.entries.filter {
            $0.sourcePath == nil ? includesUnsupportedNames : scope.admitsRawPath($0.rawPath)
        }
        guard !cancellation.isCanceled else { throw AtlasProjectIOError.canceled }
        guard let reconciled = AtlasRefreshLayout.reconcile(previous: previous, observed: visible,
            complete: catalog.discoveryComplete, retainingPreviousMembership: retainingPreviousMembership,
            reservedRightEdge: Double(previousBounds.maxX), pack: { AtlasParcelLayout.compact($0) }) else {
            throw AtlasProjectIOError.limit
        }
        guard !cancellation.isCanceled else { throw AtlasProjectIOError.canceled }
        guard let layout = AtlasViewportLayout.prepare(root: root, entries: reconciled.entries, revision: UUID()) else {
            throw AtlasProjectIOError.invalidResponse
        }
        var bounds = previousBounds, added = CGRect.null
        let files = reconciled.entries.compactMap { entry -> AtlasFile? in
            let rect = CGRect(x: entry.x, y: entry.y, width: entry.width, height: entry.height)
            bounds = bounds.union(rect)
            if reconciled.addedRawPaths.contains(entry.rawPath) { added = added.union(rect) }
            guard let path = entry.sourcePath else { return nil }
            return AtlasFile(path: path, x: entry.x, y: entry.y, w: entry.width, h: entry.height,
                bytes: Int(clamping: entry.observedBytes), lineCount: 0, sourceLineCount: nil,
                profileState: "disabled", profile: [], avgColor: AtlasFile.averageColor(profile: []))
        }
        guard !cancellation.isCanceled else { throw AtlasProjectIOError.canceled }
        return Self(catalog: catalog, visibleCatalog: visible, reconciliation: reconciled, layout: layout,
            files: files, bounds: bounds, addedBounds: added.isNull ? nil : added,
            retainingPreviousMembership: retainingPreviousMembership)
    }

    var notice: String {
        var text = "Atlas refreshed · \(reconciliation.entries.count) known files"
        if !reconciliation.addedRawPaths.isEmpty { text += " · \(reconciliation.addedRawPaths.count) added" }
        if reconciliation.removedCount > 0 { text += " · \(reconciliation.removedCount) no longer listed" }
        if !reconciliation.unconfirmedRawPaths.isEmpty {
            text += ". Partial discovery: \(reconciliation.unconfirmedRawPaths.count) earlier parcels were not re-observed and remain unconfirmed"
        } else if !catalog.discoveryComplete { text += ". Discovery remains incomplete" }
        text += retainingPreviousMembership ? ". Existing parcels and navigation stay in place."
            : ". Atlas membership now follows the selected discovery scope. Navigation is preserved."
        return text + " Visible source previews are observed again; open readers keep their captured bytes."
    }
}

@MainActor struct ContentView: View {
#if FCB_APP_STORE
    @State private var root = ""
    @State private var rootAccess: AppStoreRootAccess?
    @State private var rootAccessLoader = AtlasRootAccessLoader<AppStoreRootAccess> {
        try AppStoreRootAccess.resolve($0, cancellation: $1)
    }
    @State private var appStoreStartupConsumed = false
#else
    @State private var root = ""
    @State private var projectStartup = AtlasProjectStartup()
#endif
    @State private var nativeOpen = AtlasNativeOpen<AtlasNativeFileReader>.native()
    @State private var nativeDrop = AtlasNativeDrop<NSItemProvider> { provider, complete in
        provider.loadObject(ofClass: NSURL.self) { value, _ in
            if let value = value as? NSURL { complete(.success(value as URL)) }
            else { complete(.failure(.invalidSelection)) }
        }
    }
    @Environment(\.openWindow) private var openWindow
    @State private var query = ""
    @FocusState private var searchFocused: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var searchReport: AtlasSearchReport?
    @State private var searchPresentation: AtlasSearchPresentation?
    @State private var searchPending = false
    @State private var searchWindowPending = false
    // Created on the first search request, not on every SwiftUI view rebuild.
    @State private var searchCoordinator: AtlasSearchCoordinator?
    @State private var workspaceSearch: AtlasWorkspaceSearch?
    @State private var useCapturedIndex = false
    @State private var searchMode: AtlasSearchMode = .text
    @State private var searchNameMode: AtlasSearchNameMode = .contains
    @State private var searchCaseSensitive = true
    @State private var searchCountAllMatches = true
    @State private var resolvedMatches: [SearchHit.ID: AtlasMatch] = [:]
    @State private var searchRows: [CGRect] = []
    @State private var searchPaths: Set<String> = []
    @State private var overlayLimited = false
    @State private var overlayContentRevision: UUID?
    @State private var showsSidebar = true
    @State private var showsFileFinder = false
    @State private var projectOpening = AtlasProjectOpening()
    @State private var projectBrowser = AtlasProjectBrowserModel(transport: .native)
    @State private var projectChanges = AtlasProjectChanges(makeDriver: { AtlasNativeProjectChangeDriver() })
    @State private var projectRefresh = AtlasProjectRefresh(transport: .native)
    @State private var liveRefresh = AtlasProjectLiveRefresh()
    @State private var liveRefreshEnabled = false
    @State private var automaticRefreshProject: UUID?
    @State private var projectRefreshIO = AtlasProjectIO()
    @State private var refreshOriginProject: UUID?
    // Readers in Project Files belong to this root/grant lifetime, not to one
    // discovery session. A same-root refresh replaces rows without remounting
    // the sheet that owns an already accepted source capture.
    @State private var projectViewIdentity = UUID()
    @State private var projectViewActive = false
    @State private var browserProject = UUID()
    @State private var showsCatalog = true
    @State private var catalogEntries: [AtlasProjectCatalog.Entry] = []
    @State private var loadProgress = ""
    @State private var loadTask: Task<Void, Never>?
    @State private var projectIO = AtlasProjectIO()
    @State private var loadCancellation: AtlasSearchCancellation?
    @State private var showsReader = false
    @State private var usesPagedReader = false
    @State private var readerHistory = AtlasWorkspaceReaderHistory()
    @State private var markdownPreview = AtlasMarkdownPreviewPreference()
    @State private var observedReader: AtlasObservedReaderPresentation?
    @State private var workspaceStreamSearch: AtlasWorkspaceStreamPresentation?
    @State private var workspaceStructureSearch: AtlasWorkspaceStructurePresentation?
    @State private var projectLinks: AtlasProjectLinksPresentation?
    @State private var readingDesk = AtlasReadingDeskModel(transport: .native, recovery: .local)
    @State private var showsReadingDesk = false
    @State private var reviewDeskAfterSheetDismissal = false
    @State private var savedRepository = AtlasSavedRepositoryModel(transport: .native, trails: .native)
    @State private var snapshotExport = AtlasSnapshotExportModel(transport: .native)
    @State private var showsSavedRepository = false
    @State private var showsSnapshotComparison = false
    @State private var pinnedReaders = AtlasPinnedReaders<AtlasPagedReaderModel>(
        canFork: { $0.canPin }, fork: { try await $0.fork() }, retire: { $0.close() },
        failureMessage: { ($0 as? AtlasReaderError)?.message ?? "The source could not be pinned." })
    @State private var hits: [SearchHit] = []
    @State private var selectedHit: AtlasSearchRowID?
    @State private var searchTitle = "Search project"
    @State private var searchSummary = "Enter text to search the project."
    @State private var files: [AtlasFile] = []
    @State private var fileScope: AtlasFileScope = .all
    @State private var customExtensions = ""
    @State private var searchPathKind: AtlasSearchPathScope.Kind = .workspace
    @State private var searchPathInput = ""
    @State private var visibleSearchView: AtlasVisibleSearchScope.View?
    @State private var frozenVisibleSearch: AtlasFrozenViewportSearch?
    @State private var visibleSearchIO = AtlasProjectIO()
    @State private var visibleSearchTask: Task<Void, Never>?
    @State private var visibleSearchCancellation: AtlasSearchCancellation?
    @State private var visibleSearchRequest = UUID()
    @State private var visibleSearchNotice = ""
    @State private var displayedFiles: [AtlasFile] = []
    @State private var selectedPath: String?
    @State private var fileText = ""
    @State private var selectedSource: AtlasSource?
    @State private var selectedReaderSelection: AtlasReaderSelection?
    @State private var retainedReaderPresentation: (navigation: UUID, selection: AtlasReaderSelection?)?
    @State private var wrapsSourceLines = false
    @State private var showsSourceLineNumbers = true
    @State private var showsSourceWhitespace = false
    @State private var sourceIO = AtlasProjectIO()
    @State private var sourceTask: Task<Void, Never>?
    @State private var sourceCancellation: AtlasSearchCancellation?
    @State private var sourceSearchRow: AtlasSearchRowID?
    @State private var atlasResidency = AtlasViewportResidency(markdown: .native)
    @State private var atlasEntries: [AtlasProjectCatalog.Entry] = []
    @State private var showsCity = false
    @State private var atlasScopeLabel = "All"
    @State private var atlasDirectoryTarget: AtlasDirectoryFocus?
    @State private var atlasFolderNavigation = AtlasFolderNavigation()
    @State private var atlasRestoredScope: (revision: UUID, destination: AtlasFolderDestination)?
    @State private var atlasLandmarkDraft: (window: UUID, destination: AtlasFolderDestination)?
    @State private var atlasLandmarkName = ""
    @State private var showsAtlasLandmarkName = false
    @State private var atlasLayoutRequest = UUID()
    @State private var atlasLayoutIO = AtlasProjectIO()
    @State private var atlasLayoutTask: Task<Void, Never>?
    @State private var atlasLayoutCancellation: AtlasSearchCancellation?
    @State private var atlasLayoutPending = false
    @State private var projectCache: AtlasProjectCache?
    @State private var atlasRevision = UUID()
    @State private var retainedAtlasRevision: UUID?
    @State private var atlasPreservesNavigation = false
    @State private var atlasRefreshSummary = ""
    @State private var atlasAddedBounds: CGRect?
    @State private var atlasUnconfirmedPaths = Set<[UInt8]>()
    @State private var cityPresentedCamera: AtlasCityCamera?
    @State private var cityRefreshCamera: AtlasCityCamera?
    @State private var addedFilesFocusRevision: UUID?
    @State private var focusRequest = UUID()
    @State private var selectedMatch: AtlasMatch?
    @State private var sourceError: String?
    @State private var status = "Choose a project, then search exact text or click any file."
    @State private var contentBounds = CGRect(x: 0, y: 0, width: 4096, height: 4096)
    @State private var camera: AtlasCamera = {
        let camera = AtlasCamera()
        camera.fillsViewport = true
        return camera
    }()

    private var atlasDirectory: AtlasDirectoryFocus { projectOpening.catalog?.requestedDirectory ?? .project }
    private var loadingProject: Bool { projectOpening.isDiscovering }
    private var loadGeneration: UUID { projectOpening.projectID }
    private var atlasDocuments: [String: AtlasDocument] { atlasResidency.documents }
    private var textTiles: [AtlasTextTile] { atlasResidency.tiles }
    private var hasMetadataAtlas: Bool {
        atlasResidency.hasInstalledLayout(root: root, layoutRevision: atlasRevision)
    }
    private var hasRetainedAtlas: Bool { hasMetadataAtlas && retainedAtlasRevision == atlasRevision }
    private var workspaceReader: AtlasWorkspaceReaderCapture? {
        usesPagedReader ? readerHistory.current : nil
    }
    private var canPresentWorkspacePreview: Bool {
        showsReader && projectViewActive && !loadingProject && !readerHistory.busy
            && !nativeOpen.showsReader && !showsFileFinder && !showsReadingDesk
            && !showsSavedRepository && !showsSnapshotComparison
            && projectLinks == nil && observedReader == nil && workspaceStreamSearch == nil && workspaceStructureSearch == nil
    }
    private var hasSourcePane: Bool {
        selectedPath != nil || readerHistory.current != nil || readerHistory.busy || !readerHistory.notice.isEmpty
    }
    private var retainedReaderSelection: AtlasReaderSelection? {
        guard retainedReaderPresentation?.navigation == focusRequest else { return nil }
        return retainedReaderPresentation?.selection
    }
    private var snapshotExportSource: AtlasSnapshotExportSource? {
        guard !loadingProject, projectOpening.canBrowse, browserProject == loadGeneration,
              !root.isEmpty, projectBrowser.root.utf8.elementsEqual(root.utf8),
              let accepted = projectBrowser.info, accepted.owner > 0 else { return nil }
#if FCB_APP_STORE
        guard !rootAccessLoader.isLoading, let rootAccess,
              rootAccess.url.path.utf8.elementsEqual(root.utf8) else { return nil }
        return AtlasSnapshotExportSource(root: root, project: browserProject,
            browserOwner: accepted.owner, access: rootAccess)
#else
        return AtlasSnapshotExportSource(root: root, project: browserProject,
            browserOwner: accepted.owner, access: nil)
#endif
    }

    private var selectedObservedSource: AtlasObservedReaderSource? {
        guard let authority = snapshotExportSource,
              let raw = workspaceReader?.relativePath ?? selectedPath.map({ Array($0.utf8) }),
              let input = try? AtlasObservedReaderInput(root: authority.root, relativePath: raw) else { return nil }
        return AtlasObservedReaderSource(input: input, project: authority.identity.project, access: authority.access)
    }

    private func closeObservedReader() {
        observedReader?.close()
        observedReader = nil
    }

    private func openWorkspaceStreamSearch() {
        guard let authority = snapshotExportSource else { return }
        let pathScope: AtlasSearchPathScope
        let types: AtlasSearchFileScope
        do {
            if searchPathKind == .visible {
                guard let frozen = currentVisibleSearch else {
                    status = "Choose the displayed atlas view before opening large-file search."
                    return
                }
                pathScope = frozen.pathScope
            } else {
                pathScope = try AtlasSearchPathScope(kind: searchPathKind,
                    path: searchPathKind == .workspace ? "" : searchPathInput)
            }
            types = try fileScope.searchScope(custom: customExtensions)
        } catch {
            status = (error as? AtlasSearchError)?.message ?? "Choose a valid search location and file types."
            return
        }
        closeWorkspaceStreamSearch()
        closeWorkspaceStructureSearch()
        let source = AtlasWorkspaceStreamSource(root: authority.root,
            project: authority.identity.project, browserOwner: authority.identity.browserOwner,
            access: authority.access, fileScope: types, pathScope: pathScope)
        if searchMode == .symbols || searchMode == .headings {
            workspaceStructureSearch = AtlasWorkspaceStructurePresentation(source: source, query: query,
                mode: searchMode == .symbols ? .symbols : .headings, nameMode: searchNameMode,
                discoveryPolicy: projectBrowser.discoveryPolicy, transport: .native)
        } else {
            workspaceStreamSearch = AtlasWorkspaceStreamPresentation(source: source,
                query: query, caseSensitive: searchCaseSensitive,
                countAllMatches: searchCountAllMatches,
                mode: searchMode == .occurrences ? .occurrences : searchMode == .expression ? .expression : .text,
                transport: pathScope.isVisible ? .native : .catalogNative)
        }
    }

    private func closeWorkspaceStreamSearch() {
        workspaceStreamSearch?.close()
        workspaceStreamSearch = nil
    }

    private func closeWorkspaceStructureSearch() {
        workspaceStructureSearch?.close()
        workspaceStructureSearch = nil
    }

    private func openProjectLinks(_ direction: AtlasProjectLinkDirection = .all,
        projection: AtlasProjectRelationshipProjection = .documentation) {
        guard let authority = snapshotExportSource else { return }
        let path = direction == .all ? nil
            : (workspaceReader?.relativePath ?? selectedPath.map { Array($0.utf8) })
        guard direction == .all || path != nil else { return }
        closeProjectLinks()
        projectLinks = AtlasProjectLinksPresentation(root: authority.root,
            initialPath: path, direction: direction, projection: projection, access: authority.access)
    }

    private func closeProjectLinks() {
        projectLinks?.close()
        projectLinks = nil
    }

    private var searchContext: AtlasSearchContext {
        AtlasSearchContext(root: root, query: query, loadGeneration: loadGeneration,
            atlasRevision: atlasRevision, scope: fileScope.rawValue, customExtensions: customExtensions,
            pathKind: searchPathKind.rawValue, path: searchPathKind == .visible
                ? (currentVisibleSearch?.snapshot.id.uuidString ?? "")
                : (searchPathKind == .workspace ? "" : searchPathInput),
            mode: searchMode, nameMode: searchMode.effectiveNameMode(searchNameMode),
            caseSensitive: effectiveSearchCaseSensitive, countAllMatches: effectiveSearchCountAllMatches,
            discoveryPolicy: projectBrowser.discoveryPolicy,
            catalogKind: searchPathKind == .visible ? (currentVisibleSearch?.catalogScope.kind ?? .workspace)
                : (searchPathKind == .file ? .file : (searchPathKind == .directory ? .directory : .workspace)),
            catalogPath: searchPathKind == .visible ? (currentVisibleSearch?.catalogScope.rawPath ?? [])
                : (searchPathKind == .directory || searchPathKind == .file
                    ? Array(searchPathInput.utf8.prefix(AtlasSearchCatalogScope.maximumFileBytes + 1)) : []))
    }

    // Observe byte-distinct edits without duplicating an arbitrarily large
    // paste. Every value longer than the admitted path bound is invalid.
    private var searchPathEditKey: [UInt8] { Array(searchPathInput.utf8.prefix(4097)) }

    private var currentVisibleSearch: AtlasFrozenViewportSearch? {
        guard !atlasLayoutPending, hasMetadataAtlas, !hasRetainedAtlas, let frozenVisibleSearch,
              frozenVisibleSearch.snapshot.view.belongs(to: root, project: loadGeneration,
                layout: atlasRevision, directory: atlasDirectory) else { return nil }
        return frozenVisibleSearch
    }

    private var canFreezeVisibleSearch: Bool {
        !loadingProject && !showsCatalog && !showsCity && !atlasLayoutPending && hasMetadataAtlas
            && !hasRetainedAtlas && visibleSearchTask == nil
            && visibleSearchView?.belongs(to: root, project: loadGeneration,
                layout: atlasRevision, directory: atlasDirectory) == true
    }

    private var openFileSearchPath: String? {
        guard let path = selectedPath else { return nil }
        switch searchPathKind {
        case .workspace, .visible: return nil
        case .file: return path
        case .directory:
            guard let slash = path.lastIndex(of: "/") else { return nil }
            return String(path[..<slash])
        }
    }

    private var searchIsCurrent: Bool {
        !loadingProject && (searchReport?.indexStatus != nil) == wantsCapturedIndex
            && searchReport?.mode == searchMode
            && searchReport?.caseSensitive == effectiveSearchCaseSensitive
            && searchReport?.countAllMatches == effectiveSearchCountAllMatches
            && searchPresentation?.isCurrent(in: searchContext) == true
    }

    private var wantsCapturedIndex: Bool { useCapturedIndex }
    private var effectiveSearchCaseSensitive: Bool { !searchMode.isTextual || searchCaseSensitive }
    private var effectiveSearchCountAllMatches: Bool { searchMode.isTextual && searchCountAllMatches }
    private var overlayIsCurrent: Bool {
        !hasRetainedAtlas && searchIsCurrent && overlayContentRevision == atlasResidency.contentRevision
    }

    var body: some View {
        HStack(spacing: 0) {
            atlasScreen
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            if showsSidebar {
                sidebar.frame(width: 300).background(.bar)
            }
        }
        .frame(minWidth: 1180, minHeight: 740)
        .background(AtlasDeskCloseProtection(model: readingDesk, review: reviewReadingDeskBeforeClosing))
        .background(AtlasNativeOpenWindow(model: nativeOpen,
            newWindow: { [openWindow] in openWindow(id: "browser") }, receive: receiveNativeOpen))
        .onDrop(of: [UTType.fileURL.identifier], isTargeted: nil, perform: acceptNativeDrop)
        .safeAreaInset(edge: .bottom) {
            if !nativeOpen.showsReader { AtlasNativeOpenStatus(model: nativeOpen) }
        }
        .toolbar {
            Button { chooseNativeItem(allowFiles: true) } label: {
                Label("Open", systemImage: "folder.badge.plus")
            }
            .help("Open one file or project folder (⌘O)")
            Button {
                guard !AtlasReaderCommands.perform(.find) else { return }
                showsSidebar = true
                searchFocused = true
            } label: {
                Label("Search and Projects", systemImage: "sidebar.right")
            }
            .keyboardShortcut("f", modifiers: .command)
            .help("Find in the focused source reader, or show search and projects (⌘F)")
            Button { showsFileFinder = true } label: {
                Label("Quick Open", systemImage: "doc.text.magnifyingglass")
            }
            .keyboardShortcut("p", modifiers: .command)
            .disabled(root.isEmpty || loadingProject)
            .help("Find project files by filename or path (⌘P)")
            Button { showsCatalog = true } label: {
                Label("Project Files", systemImage: "list.bullet.rectangle")
            }
            .disabled(!projectOpening.canBrowse)
            .help("Browse known files without waiting for text-atlas previews")
            Menu {
                Toggle("Live refresh", isOn: $liveRefreshEnabled)
                Button("Refresh project", action: requestAtlasLoad)
                    .disabled(!projectOpening.canBrowse || loadingProject || projectRefresh.isRefreshing)
            } label: {
                Label(liveRefreshEnabled ? "Live refresh on" : "Refresh", systemImage: "arrow.triangle.2.circlepath")
            }
            .help("Keep this window's project catalog, atlas and project search current after filesystem changes. Open captured readers keep their bytes.")
            Picker("Atlas projection", selection: $showsCity) {
                Text("2D").tag(false)
                Text("City").tag(true)
            }
            .pickerStyle(.segmented).frame(width: 110)
            .disabled(!hasMetadataAtlas || atlasLayoutPending)
            .help("Explore the same file footprints in 2D or as buildings sized by observed file bytes")
            Menu {
                Picker("Files and search scope", selection: $fileScope) {
                    ForEach(AtlasFileScope.allCases) { scope in Text(scope.rawValue).tag(scope) }
                }
            } label: { Label(fileScope.rawValue, systemImage: "line.3.horizontal.decrease.circle") }
            .help("Choose file types for the atlas and workspace search")
            Button {
                showsReader.toggle()
            } label: {
                Label("Source Reader", systemImage: "doc.text")
            }
            .disabled(!hasSourcePane)
            .help("Show or hide the selectable source reader")
            Button(action: toggleMarkdownPreview) {
                Label(markdownPreview.enabled ? "Markdown preview on" : "Markdown preview", systemImage: "doc.richtext")
            }
            .keyboardShortcut("r", modifiers: .command)
            .help("Toggle Markdown previews in atlas parcels and the retained reader (⌘R). Atlas previews reuse captured bytes and upstream layout; Source returns to those same bytes.")
            Button { showsReadingDesk = true } label: {
                Label("Reading Desk", systemImage: "books.vertical")
            }
            .help("Read retained sources, follow cross-file history, keep named bookmarks, and save or open a checkpoint")
            Button { showsSavedRepository = true } label: {
                Label("Saved Repository", systemImage: "archivebox")
            }
            .help("Save the current project as an FCBS archive, or read saved members, documentation links and backlinks")
            Button { showsSnapshotComparison = true } label: {
                Label("Compare Captures", systemImage: "arrow.left.arrow.right")
            }
            .help("Compare two saved project captures and open exact before/after sources")
            Button { openProjectLinks() } label: {
                Label("Relationships", systemImage: "link")
            }
            .disabled(snapshotExportSource == nil)
            .help("Capture project sources and follow documentation links or lexical import candidates with retained evidence")
            Button {
                guard !AtlasReaderCommands.perform(.previous) else { return }
                moveSearchResult(backwards: true)
            } label: {
                Label("Previous Match", systemImage: "arrow.up")
            }
            .keyboardShortcut("g", modifiers: [.command, .shift])
            .disabled(!searchIsCurrent || hits.isEmpty)
            .help("Previous search match (⇧⌘G)")
            Button {
                guard !AtlasReaderCommands.perform(.next) else { return }
                moveSearchResult(backwards: false)
            } label: {
                Label("Next Match", systemImage: "arrow.down")
            }
            .keyboardShortcut("g", modifiers: .command)
            .disabled(!searchIsCurrent || hits.isEmpty)
            .help("Next search match (⌘G)")
        }
        .alert("Name this atlas landmark", isPresented: $showsAtlasLandmarkName) {
            TextField("Landmark name", text: $atlasLandmarkName)
            Button("Save", action: saveAtlasLandmark)
            Button("Cancel", role: .cancel) { atlasLandmarkDraft = nil }
        } message: {
            Text("Keep this folder, file filter and map position in this window. Landmarks last until you close or replace the project.")
        }
        .sheet(isPresented: $nativeOpen.showsReader) {
            if let presentation = nativeOpen.presentationID {
                AtlasNativeOpenView(model: nativeOpen, presentationID: presentation,
                    desk: readingDesk, chooseProject: chooseProject).id(presentation)
            }
        }
        .sheet(isPresented: $showsFileFinder, onDismiss: finishReadingDeskReview) { fileFinderSheet }
        .sheet(isPresented: $showsReadingDesk) { AtlasReadingDeskView(model: readingDesk, project: projectBrowser) }
        .sheet(isPresented: $showsSavedRepository, onDismiss: finishReadingDeskReview) {
            AtlasSavedRepositoryView(model: savedRepository, desk: readingDesk, exporter: snapshotExport,
                currentSource: { snapshotExportSource })
        }
        .sheet(isPresented: $showsSnapshotComparison, onDismiss: finishReadingDeskReview) {
            AtlasSnapshotComparisonView(desk: readingDesk)
        }
        .sheet(item: $projectLinks, onDismiss: finishReadingDeskReview) { presentation in
            AtlasProjectLinksView(presentation: presentation, desk: readingDesk).id(presentation.id)
                .onDisappear { presentation.close() }
        }
        .sheet(item: $observedReader, onDismiss: finishReadingDeskReview) { presentation in
            AtlasObservedReaderView(presentation: presentation, desk: readingDesk,
                searchProject: observedSourceProjectSearch(for: presentation)).id(presentation.id)
        }
        .sheet(item: $workspaceStreamSearch, onDismiss: {
            closeWorkspaceStreamSearch()
            finishReadingDeskReview()
        }) { presentation in
            AtlasWorkspaceStreamView(presentation: presentation, desk: readingDesk).id(presentation.id)
                .onDisappear { presentation.close() }
        }
        .sheet(item: $workspaceStructureSearch, onDismiss: finishReadingDeskReview) { presentation in
            AtlasWorkspaceStructureView(presentation: presentation).id(presentation.id)
                .onDisappear { presentation.close() }
        }
        .onAppear {
            projectViewActive = true
            nativeOpen.cancelExternal = { [weak nativeDrop] in nativeDrop?.cancel() }
            nativeOpen.activate()
#if FCB_APP_STORE
            if rootAccess != nil { requestAtlasLoad() }
            else if !appStoreStartupConsumed {
                appStoreStartupConsumed = true
                if let recent = Engine.recents.first { restoreProject(recent) }
                else { status = "Choose a project folder to begin exploring its source." }
            }
#else
            if !projectStartup.consumed,
               let selection = projectStartup.consume(arguments: CommandLine.arguments,
                restoredRoot: UserDefaults.standard.string(forKey: "fcb.root") ?? Engine.defaultRoot) {
                switch selection {
                case .open(let initialRoot, let remember):
                    root = initialRoot
                    if remember {
                        UserDefaults.standard.set(initialRoot, forKey: "fcb.root")
                        Engine.remember(root: initialRoot)
                    }
                case .chooseProject:
                    root = ""
                case .invalidRoot:
                    root = ""
                    status = "The startup project path is invalid. Choose a project folder to begin."
                }
            }
            if !root.isEmpty { requestAtlasLoad() }
#endif
            AtlasNativeOpenRouting.shared.flush(createWindow: false)
        }
        .onDisappear {
            projectViewActive = false
            nativeOpen.close()
            liveRefresh.close()
            automaticRefreshProject = nil
            projectRefresh.cancel()
            refreshOriginProject = nil
            projectViewIdentity = UUID()
            resetAtlasFolderNavigation()
            retainedReaderPresentation = nil
#if FCB_APP_STORE
            rootAccessLoader.cancel()
#endif
            projectChanges.close()
            closeAtlasResidency()
            readerHistory.close()
            projectBrowser.close()
            savedRepository.close()
            snapshotExport.cancel()
            closeObservedReader()
            closeWorkspaceStreamSearch()
            closeWorkspaceStructureSearch()
            closeProjectLinks()
            showsSavedRepository = false
            showsSnapshotComparison = false
            readingDesk.close()
            showsReadingDesk = false
            pinnedReaders.closeAll()
            showsFileFinder = false
            cancelSourceLoad()
            loadTask?.cancel()
            loadCancellation?.cancel()
            projectIO.cancel()
            projectOpening.close()
            loadTask = nil
            loadCancellation = nil
            catalogEntries = []
            clearSearch()
            searchCoordinator = nil
            workspaceSearch?.refresh()
            workspaceSearch = nil
        }
        .onChange(of: showsCatalog) { _, showing in
            if showing { atlasResidency.suspend() }
            visibleSearchView = nil
            // Returning creates a new native surface whose accepted frame
            // supplies fresh viewport demand; hidden views do no enrichment.
        }
        .onChange(of: markdownPreview.enabled) { _, enabled in
            atlasResidency.setMarkdownPreviewEnabled(enabled)
        }
        .onChange(of: readerHistory.current?.id) { _, visit in
            markdownPreview.activate(visit)
            if readerHistory.current != nil { presentWorkspaceReader() }
        }
        .onChange(of: snapshotExportSource?.identity) { _, _ in
            bindWorkspaceReaderHistory()
            snapshotExport.cancel()
            closeObservedReader()
            closeWorkspaceStreamSearch()
            closeWorkspaceStructureSearch()
            closeProjectLinks()
        }
        .onChange(of: atlasResidency.contentRevision) { _, _ in
            refreshResidentSearchOverlay()
        }
        .onChange(of: focusRequest) { _, _ in
            if retainedReaderPresentation?.navigation != focusRequest { retainedReaderPresentation = nil }
        }
        .onChange(of: useCapturedIndex) { _, _ in
            clearSearch()
            workspaceSearch?.configure(indexed: wantsCapturedIndex)
            runSearch(debounce: true)
        }
        .onChange(of: searchMode) { _, _ in
            clearSearch()
            workspaceSearch?.configure(indexed: wantsCapturedIndex)
            runSearch(debounce: true)
        }
        .onChange(of: searchNameMode) { _, _ in
            if searchMode.supportsNameMatching {
                clearSearch()
                runSearch(debounce: true)
            }
        }
        .onChange(of: searchCaseSensitive) { _, _ in
            if searchMode.isTextual {
                clearSearch()
                // The captured source index is independent of query case.
                // Keep it; only replace the query and its visible authority.
                runSearch(debounce: true)
            }
        }
        .onChange(of: searchCountAllMatches) { _, _ in
            if searchMode.isTextual {
                clearSearch()
                runSearch(debounce: true)
            }
        }
        .onChange(of: pinnedReaders.notice) { _, notice in
            if !notice.isEmpty && !pinnedReaders.busy { status = notice }
        }
        .onChange(of: projectBrowser.info?.owner) { _, _ in
            // Manual recovery after a lost initial response can publish rows
            // after the original opening task failed. Only this exact current
            // project/browser/root may make source search and previews ready.
            guard !projectOpening.isDiscovering, projectBrowser.currentPage, !projectBrowser.closed,
                  projectBrowser.root.utf8.elementsEqual(root.utf8),
                  projectOpening.recoverProgressive(in: browserProject) else { return }
            status = projectBrowser.notice
            if !query.isEmpty { runSearch(debounce: true) }
            if AtlasFileStartup.isPending { openStartupFile() }
        }
        .onChange(of: projectChangeDiscoveryKey) { _, _ in
            guard !projectBrowser.closed, projectBrowser.root.utf8.elementsEqual(root.utf8),
                  projectOpening.projectID == browserProject, let info = projectBrowser.info else { return }
            projectChanges.reportDiscovery(project: browserProject, complete: info.complete,
                more: info.more, omissions: info.omissionCount)
        }
        .onChange(of: liveRefreshInput, initial: true) { _, input in
            liveRefresh.update(input) { ticket in
                guard liveRefreshInput.admits(ticket), projectChanges.acceptsAutomaticRefresh(ticket) else { return false }
                return requestProjectRefresh(automatic: ticket)
            }
        }
        .onChange(of: liveRefreshEnabled) { _, enabled in
            if !enabled, automaticRefreshProject != nil { projectRefresh.cancel() }
        }
        .onChange(of: projectRefresh.isRefreshing) { _, refreshing in
            if !refreshing { automaticRefreshProject = nil }
        }
        .onChange(of: Array(query.utf8)) { _, _ in
            // Revoke old rows immediately, including empty or invalid edits.
            clearSearch()
            runSearch(debounce: true)
        }
        .onChange(of: Array(customExtensions.utf8)) { _, _ in
            guard !isRestoredAtlasScope else { return }
            atlasRestoredScope = nil
            if fileScope == .custom { clearSearch() }
        }
        .onChange(of: searchPathKind) { _, _ in
            clearSearch()
            if searchPathKind == .visible { freezeVisibleSearch() }
            else {
                cancelVisibleSearchFreeze()
                runSearch(debounce: true)
            }
        }
        .onChange(of: searchPathEditKey) { _, _ in
            if searchPathKind == .directory || searchPathKind == .file {
                clearSearch()
                runSearch(debounce: true)
            }
        }
        .onChange(of: fileScope) { _, value in
            guard !isRestoredAtlasScope else { return }
            atlasRestoredScope = nil
            if value == .custom { showsSidebar = true }
            applyFileScope()
        }
        .onChange(of: showsCity) { _, city in
            camera.stopMotion()
            cityRefreshCamera = nil
            if !city { cityPresentedCamera = nil }
            if city { showsCatalog = false }
            invalidateVisibleSearchScope()
            if city && searchPathKind == .visible {
                visibleSearchNotice = "Return to 2D to choose a visible-file search scope. Other search scopes remain available in City."
            }
            if !city {
                if addedFilesFocusRevision == atlasRevision, let bounds = atlasAddedBounds {
                    _ = camera.navigateFocus(bounds: bounds, revision: atlasRevision)
                }
                addedFilesFocusRevision = nil
            }
        }
        .onChange(of: selectedHit) { _, selected in
            guard let selected else { return }
            // Admit this report's immutable witness, not a reusable integer.
            // Exact source verification happens before installing the reader;
            // missing atlas geometry must not disable on-demand navigation.
            guard searchIsCurrent,
                  let hit = searchPresentation?.captureCandidate(for: selected, in: searchContext),
                  let path = hit.sourcePath else {
                selectedHit = nil
                selectedMatch = nil
                selectedReaderSelection = nil
                status = (searchPresentation?.availability(for: selected, in: searchContext) ?? .stale).message
                return
            }
            openFile(path, searchRow: selected, exactMatch: true)
            showsReader = true
        }
    }

    private func reviewReadingDeskBeforeClosing() {
        guard !readingDesk.closed, !showsReadingDesk else { return }
        if showsFileFinder || showsSavedRepository || showsSnapshotComparison || observedReader != nil || workspaceStreamSearch != nil || workspaceStructureSearch != nil || projectLinks != nil {
            // Wait for the other sheet's actual dismissal before presenting
            // the desk. An active note editor is preserved by the close guard.
            reviewDeskAfterSheetDismissal = true
            showsFileFinder = false; showsSavedRepository = false; showsSnapshotComparison = false; closeObservedReader()
            closeWorkspaceStreamSearch()
            closeWorkspaceStructureSearch()
            closeProjectLinks()
        } else { showsReadingDesk = true }
    }
    private func finishReadingDeskReview() {
        guard reviewDeskAfterSheetDismissal else { return }
        reviewDeskAfterSheetDismissal = false
        if !readingDesk.closed { showsReadingDesk = true }
    }

    // Quick Open owns a progressive query independently of the atlas and
    // content search. Its selected row hands over an already captured reader.
    @ViewBuilder private var fileFinderSheet: some View {
#if FCB_APP_STORE
        if let access = rootAccess, access.url.path.utf8.elementsEqual(root.utf8) {
            fileFinder(accessLease: access)
        } else {
            Text("Choose the project folder again to grant read access.").padding(20)
        }
#else
        fileFinder(accessLease: nil)
#endif
    }

    private func fileFinder(accessLease: AnyObject?) -> some View {
        let requestedRoot = root
        let generation = loadGeneration
        return AtlasProgressiveQuickOpenView(root: requestedRoot, project: generation, accessLease: accessLease,
            desk: readingDesk,
            canObserveCurrentProject: {
                guard showsFileFinder, !loadingProject, loadGeneration == generation,
                      root.utf8.elementsEqual(requestedRoot.utf8) else { return false }
#if FCB_APP_STORE
                guard !rootAccessLoader.isLoading,
                      rootAccess.map(ObjectIdentifier.init) == accessLease.map(ObjectIdentifier.init),
                      rootAccess?.url.path.utf8.elementsEqual(requestedRoot.utf8) == true else { return false }
#endif
                return true
            }) { capture in
            guard showsFileFinder, !loadingProject, loadGeneration == generation,
                  root.utf8.elementsEqual(requestedRoot.utf8),
                  capture.root.utf8.elementsEqual(requestedRoot.utf8),
                  capture.reader.page?.identity.pathBytes == capture.file.rawPath else {
                capture.reader.close(); return
            }
#if FCB_APP_STORE
            guard !rootAccessLoader.isLoading,
                  rootAccess.map(ObjectIdentifier.init) == accessLease.map(ObjectIdentifier.init),
                  rootAccess?.url.path.utf8.elementsEqual(requestedRoot.utf8) == true else {
                capture.reader.close(); return
            }
#endif
            cancelSourceLoad()
            selectedHit = nil
            readerHistory.adopt(root: requestedRoot, relativePath: capture.file.rawPath,
                label: capture.file.display, accessLease: accessLease, reader: capture.reader)
            showsFileFinder = false
            showsReader = true
            status = "Opening the selected retained filename in source history."
        }
        .id(generation)
    }

    // MARK: Sidebar

    private var searchLocationControls: some View {
        VStack(alignment: .leading, spacing: 6) {
            Picker("Search location", selection: $searchPathKind) {
                ForEach(AtlasSearchPathScope.Kind.allCases) { kind in
                    Text(kind.label).tag(kind)
                }
            }
            .pickerStyle(.segmented)
            .accessibilityLabel("Search location")
            .help("Search the workspace, a folder and its descendants, one exact file, or files from a frozen atlas view; file-type filters also apply")
            if searchPathKind == .visible {
                HStack {
                    Button("Use current view", action: freezeVisibleSearch)
                        .disabled(!canFreezeVisibleSearch)
                        .accessibilityLabel("Freeze the currently displayed atlas files as the search scope")
                    if visibleSearchTask != nil {
                        ProgressView().controlSize(.small)
                        Button("Cancel", action: cancelChoosingVisibleSearch)
                    }
                    Spacer()
                    if showsCatalog && hasMetadataAtlas {
                        Button("Show text atlas") { showsCatalog = false }
                    }
                }
                if let frozen = currentVisibleSearch {
                    Text("\(frozen.snapshot.count) files in the frozen atlas view")
                        .font(.caption.weight(.semibold))
                        .accessibilityLabel("\(frozen.snapshot.count) files in the frozen atlas search scope")
                    if let visibleSearchView,
                       !frozen.snapshot.view.represents(root: root, project: loadGeneration,
                            layout: atlasRevision, viewport: visibleSearchView.viewport, directory: atlasDirectory) {
                        Text("The atlas view has moved. Search still uses these frozen files; use the current view to change the scope.")
                            .font(.caption2).foregroundStyle(.secondary)
                    }
                } else if visibleSearchTask == nil {
                    Text(hasRetainedAtlas ? "Rebuild the earlier atlas before choosing its visible files."
                        : (hasMetadataAtlas ? "Show the text atlas, then choose its current view."
                            : "Build the text atlas to choose its visible files."))
                        .font(.caption2).foregroundStyle(.secondary)
                }
                if !visibleSearchNotice.isEmpty {
                    Text(visibleSearchNotice).font(.caption2).foregroundStyle(.secondary).textSelection(.enabled)
                }
                Text("Search whole files whose parcels intersect the chosen view, including files without loaded previews. Camera movement does not change the frozen scope.")
                    .font(.caption2).foregroundStyle(.secondary)
            } else if searchPathKind == .directory || searchPathKind == .file {
                TextField(searchPathKind == .file ? "Relative file path, e.g. src/main.rs"
                    : "Relative folder path, e.g. src", text: $searchPathInput)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(.caption, design: .monospaced))
                    .accessibilityLabel(searchPathKind == .file ? "Exact project-relative file path"
                        : "Project-relative search folder")
                    .onSubmit(runSearch)
                Button(searchPathKind == .file ? "Use open file" : "Use open file's folder") {
                    if let path = openFileSearchPath { searchPathInput = path }
                }
                .disabled(openFileSearchPath == nil)
                .help(searchPathKind == .file ? "Copy the open file's path into this search"
                    : "Copy its containing folder. Choose Workspace for a file at the project root.")
                Text("Search stays in this \(searchPathKind.label.lowercased()) when you open another file.")
                    .font(.caption2).foregroundStyle(.secondary)
            }
        }
        .disabled(loadingProject || root.isEmpty)
        .padding(.horizontal, 10).padding(.top, 8)
    }

    private var sidebar: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Search project").font(.headline)
                Spacer()
                Button { showsSidebar = false; searchFocused = false } label: {
                    Image(systemName: "sidebar.right")
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Hide search sidebar")
                .help("Hide search; Command-F opens it again")
            }.padding(10)
            Picker("Search mode", selection: $searchMode) {
                ForEach(AtlasSearchMode.allCases) { mode in Text(mode.label).tag(mode) }
            }
            .pickerStyle(.menu).padding(.horizontal, 10)
            .accessibilityLabel("Search mode")
            .help("Search literal text, expressions, declaration candidates, Markdown headings, or whole-token identifier occurrences across the selected scope")
            HStack {
                TextField(searchMode.placeholder, text: $query)
                    .textFieldStyle(.roundedBorder)
                    .focused($searchFocused)
                    .onSubmit(runSearch)
                    .onExitCommand { query = ""; clearSearch() }
                Button("Search") { runSearch() }
                    .disabled(loadingProject || root.isEmpty || query.isEmpty)
                if !query.isEmpty {
                    Button { query = ""; clearSearch() } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Clear search")
                }
            }
            .padding(.horizontal, 10)
            .padding(.top, 10)
            if searchMode.isTextual {
                Toggle("Match case", isOn: $searchCaseSensitive)
                    .toggleStyle(.checkbox)
                    .font(.caption)
                    .disabled(loadingProject || root.isEmpty)
                    .help("When off, compare using Unicode lowercase. Results retain the original source spelling and byte ranges.")
                    .padding(.horizontal, 10).padding(.top, 6)
                Toggle("Count all matches", isOn: $searchCountAllMatches)
                    .toggleStyle(.checkbox)
                    .font(.caption)
                    .disabled(loadingProject || root.isEmpty)
                    .help("Continue counting after the first 1,000 results. Source and scan limits still apply; incomplete totals are labeled partial.")
                    .padding(.horizontal, 10).padding(.top, 4)
            }
            if searchMode.supportsNameMatching {
                Picker("Name matching", selection: $searchNameMode) {
                    ForEach(AtlasSearchNameMode.allCases) { mode in Text(mode.label).tag(mode) }
                }
                .pickerStyle(.segmented).padding(.horizontal, 10).padding(.top, 6)
                .accessibilityLabel("Name matching")
            }
            if !searchMode.isTextual {
                Text(searchMode.explanation)
                    .font(.caption2).foregroundStyle(.secondary)
                    .padding(.horizontal, 10).padding(.top, 4)
            }
            if searchMode == .expression {
                VStack(alignment: .leading, spacing: 4) {
                    Text(searchMode.explanation)
                    Text("Example: \"pub fn\" Result -unsafe path:src/ lang:rust")
                        .font(.system(.caption2, design: .monospaced)).textSelection(.enabled)
                    Text("Path and language filters narrow the selected location and file types. Regex is unavailable.")
                }
                .font(.caption2).foregroundStyle(.secondary)
                .padding(.horizontal, 10).padding(.top, 6)
            }
            searchLocationControls
            HStack {
                Toggle("Captured index", isOn: Binding(
                    get: { wantsCapturedIndex }, set: { useCapturedIndex = $0 }))
                    .toggleStyle(.checkbox)
                    .disabled(loadingProject)
                    .help("Capture the current file and location scope once, then search those retained bytes until Refresh.")
                Spacer()
                Button("Refresh", action: refreshCapturedSearch)
                    .disabled(!wantsCapturedIndex || loadingProject || root.isEmpty)
                    .help("Capture the current file and location scope into a new index. Opened readers keep their original bytes.")
            }
            .font(.caption).padding(.horizontal, 10).padding(.top, 6)
            Button(action: openWorkspaceStreamSearch) {
                Label(searchMode == .symbols || searchMode == .headings
                    ? "Search project structure…" : "Search large project files…", systemImage: "doc.text.magnifyingglass")
            }
            .disabled(snapshotExportSource == nil)
            .help(searchMode == .symbols || searchMode == .headings
                ? "Discover symbols or Markdown headings beyond the atlas file-count limit. The sheet keeps the selected location, file types and discovery policy fixed and reports sources it cannot analyze."
                : "Search text with optional Unicode lowercase matching in the selected location and file types, including sources beyond capture limits. The sheet keeps this scope fixed and offers a total I/O budget.")
            .padding(.horizontal, 10).padding(.top, 8)
            if wantsCapturedIndex {
                Text("The first search captures the selected scope. Later queries reuse those bytes; Refresh captures the current scope and file edits. Limits within that scope: 4096 files, 1 MiB per file, 32 MiB total.")
                    .font(.caption2).foregroundStyle(.secondary)
                    .padding(.horizontal, 10).padding(.top, 4)
                if let index = searchReport?.indexStatus, !index.building {
                    Text("\(index.reused ? "Reused" : "Prepared") index for \(index.captureScopeDescription) · \(index.capturedFiles) of \(index.captureEligibleFiles ?? index.capturedFiles) eligible files captured · \(index.capturedBytes) source bytes. This query verified \(index.verifiedFiles) files and skipped \(index.skippedFiles) by index.")
                        .font(.caption2).foregroundStyle(.secondary)
                        .padding(.horizontal, 10).padding(.top, 4)
                }
            }
            if searchPending {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("Searching project")
                    Text(searchReport?.indexStatus?.building == true ? "Preparing index…" : "Searching…").font(.caption)
                    Spacer()
                    Button("Cancel", action: cancelSearch)
                }
                .padding(.horizontal, 10)
                .padding(.top, 6)
            }
            if fileScope == .custom {
                TextField("Extensions: md, py, rs…", text: $customExtensions)
                    .textFieldStyle(.roundedBorder).padding(.horizontal, 10)
                    .onSubmit { applyFileScope() }
            }
            projectPicker
                .padding(.horizontal, 10)
                .padding(.top, 10)
            Group {
                let preparing = searchReport?.indexStatus?.building == true
                let count = preparing ? UInt64(searchReport?.indexStatus?.capturedFiles ?? 0) : (searchReport?.matchesSeen ?? 0)
                let countLabel = searchReport.map { report in
                    (report.isWorkspaceScope ? report.resultLabel
                        : (report.pathScope.isWorkspace ? report.resultLabel + " in selected types"
                            : (report.pathScope.isVisible ? report.resultLabel + " in frozen atlas files"
                                : report.resultLabel + " in selected " + report.pathScope.kind.label.lowercased())))
                        + (report.complete ? "" : (report.isInProgress ? " · searching" : " · partial"))
                } ?? "ready to search"
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(count, format: .number)
                        .font(.system(size: 30, weight: .semibold, design: .rounded))
                        .monospacedDigit()
                        .contentTransition(.numericText())
                        .animation(reduceMotion ? nil : .spring(response: 0.38, dampingFraction: 0.82), value: count)
                    Text(preparing ? "files captured" : countLabel)
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                }
                .padding(10)
                if overlayLimited {
                    Text("Some match rows are unavailable or exceed the overlay limit. File outlines show returned matches.")
                        .font(.caption).foregroundStyle(.secondary).padding(.horizontal, 10)
                }
            }
            if searchIsCurrent, let report = searchReport, !report.isInProgress,
               let window = report.resultWindow {
                VStack(alignment: .leading, spacing: 5) {
                    Text(window.rangeLabel).font(.caption).foregroundStyle(.secondary)
                    HStack {
                        Button("Previous results") {
                            if let request = window.previous { runSearch(debounce: false, window: request) }
                        }
                        .disabled(searchPending || window.previous == nil)
                        .accessibilityLabel("Previous captured search result window")
                        Spacer()
                        Button("Next results") {
                            if let request = window.next { runSearch(debounce: false, window: request) }
                        }
                        .disabled(searchPending || window.next == nil)
                        .accessibilityLabel("Next captured search result window")
                    }
                    .controlSize(.small)
                    .help("Verify another bounded result window in the same captured files. Refresh starts a new capture.")
                }
                .padding(.horizontal, 10).padding(.bottom, 8)
            }
            if query.isEmpty && !loadingProject && !displayedFiles.isEmpty {
                // Catalog membership, not an eager source/Metal tile, makes a
                // file discoverable. UTF-16 and unprepared files remain usable.
                List {
                    Section("\(hasRetainedAtlas ? "Earlier atlas files" : "Project files") · \(displayedFiles.count)") {
                        ForEach(displayedFiles, id: \.rawPathKey) { file in
                            Button {
                                selectedHit = nil
                                openFile(file.path)
                                showsReader = true
                            } label: {
                                Text(String(reflecting: file.path))
                                    .font(.system(size: 11, design: .monospaced))
                                    .lineLimit(2)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel("Open " + String(reflecting: file.path))
                            .help("Read the file even when its atlas preview is unavailable")
                        }
                    }
                }
                .listStyle(.sidebar)
            } else if hits.isEmpty {
                ContentUnavailableView {
                    Label(searchTitle, systemImage: "magnifyingglass")
                } description: {
                    Text(searchSummary)
                }
                .frame(maxHeight: .infinity)
            } else {
                Text(searchSummary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 10)
                if let presentation = searchPresentation {
                    List(selection: $selectedHit) {
                        ForEach(hits) { hit in
                            let row = presentation.rowID(for: hit.id)
                            let availability = presentation.availability(for: row, in: searchContext)
                            let canOpenFile = presentation.filePath(for: row, in: searchContext) != nil
                            HitRow(hit: hit, isSameFile: selectedPath == hit.sourcePath,
                                availability: availability,
                                openFile: canOpenFile ? { openSearchFile(row) } : nil)
                                .tag(row)
                                .disabled(!availability.allowsActivation && !canOpenFile)
                        }
                    }
                    .id(presentation.id)
                    .listStyle(.sidebar)
                }
            }
            footer
        }
    }

    private var projectPicker: some View {
        Menu {
            Button("Choose Folder…") { chooseProject() }
            if !Engine.recents.isEmpty { Divider() }
            ForEach(Engine.recents, id: \.self) { recent in
#if FCB_APP_STORE
                Button(recent) { restoreProject(recent) }
#else
                Button(recent) { setRoot(recent) }
#endif
            }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "folder.fill")
                    .foregroundStyle(Color.accentColor)
                VStack(alignment: .leading, spacing: 1) {
                    Text(root.isEmpty ? "No project selected" : (root as NSString).lastPathComponent)
                        .font(.system(size: 13, weight: .semibold))
                        .lineLimit(1)
                    Text(root.isEmpty ? "Choose a folder to begin" : "\(projectBrowser.info?.discovered ?? 0) files discovered · \(root)")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                Image(systemName: "chevron.up.chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize(horizontal: false, vertical: true)
    }

    private var footer: some View {
        Text(status)
            .font(.system(size: 11))
            .foregroundStyle(.secondary)
            .lineLimit(3)
            .padding(8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.bar)
    }

    // MARK: Atlas screen

    private var canChangeAtlasDirectory: Bool {
        projectViewActive && !loadingProject && !projectOpening.isPreparing && !projectRefresh.isRefreshing
            && !atlasLayoutPending && projectOpening.canBrowse && !projectBrowser.closed
            && browserProject == loadGeneration && projectBrowser.root.utf8.elementsEqual(root.utf8)
    }

    private var selectedAtlasDirectory: AtlasDirectoryFocus? {
        guard let selectedPath,
              let entry = atlasEntries.first(where: { $0.rawPath.elementsEqual(selectedPath.utf8) }) else { return nil }
        return try? AtlasDirectoryFocus.containingFile(rawPath: entry.rawPath)
    }

    private var isRestoredAtlasScope: Bool {
        atlasRestoredScope.map {
            $0.revision == atlasRevision
                && $0.destination.matchesScope(fileScope: fileScope, customExtensions: customExtensions)
        } ?? false
    }

    private func resetAtlasFolderNavigation() {
        atlasFolderNavigation.reset()
        atlasRestoredScope = nil
        atlasLandmarkDraft = nil
        showsAtlasLandmarkName = false
    }

    /// A place is a directory/filter plus a position relative to one current
    /// file parcel. It grants no source access and retains no catalog or capture.
    /// Resolve paths again after rebuilding; never reuse response-local file IDs.
    private func currentAtlasDestination(usingCurrentFilter: Bool = false) -> AtlasFolderDestination? {
        let accepted = atlasFolderNavigation.current
        let scope = hasMetadataAtlas && !usingCurrentFilter ? (accepted?.fileScope ?? fileScope) : fileScope
        let extensions = hasMetadataAtlas && !usingCurrentFilter ? (accepted?.customExtensions ?? customExtensions) : customExtensions
        var anchorPath: [UInt8]?
        var anchor: AtlasParcelCameraAnchor?
        if hasMetadataAtlas, !hasRetainedAtlas, let accepted, accepted.directory == atlasDirectory,
           let path = accepted.anchorRawPath, let entry = atlasEntries.first(where: { $0.rawPath == path }),
           let position = camera.pendingPresentationAnchor(relativeTo: CGRect(x: entry.x, y: entry.y,
                width: entry.width, height: entry.height), revision: atlasRevision) {
            // Keep a pending hidden view relative to its original parcel even
            // when its center lies nearer a neighbor. No viewport is invented.
            return try? AtlasFolderDestination(directory: atlasDirectory, fileScope: scope,
                customExtensions: extensions, anchorRawPath: path, cameraAnchor: position)
        }
        if hasMetadataAtlas, !hasRetainedAtlas, camera.presentationCenter(revision: atlasRevision) == nil,
           let accepted, accepted.directory == atlasDirectory {
            // A landmark can be accepted while Project Files owns the visible
            // surface. Its deferred camera intent is still the current place.
            let resolved = resolvedAtlasDestination(accepted, entries: atlasEntries)
            return try? AtlasFolderDestination(directory: atlasDirectory, fileScope: scope,
                customExtensions: extensions, anchorRawPath: resolved.anchorRawPath, cameraAnchor: resolved.cameraAnchor)
        }
        if hasMetadataAtlas, !hasRetainedAtlas,
           let center = camera.presentationCenter(revision: atlasRevision) {
            var nearest: AtlasProjectCatalog.Entry?
            var distance = Double.infinity
            for entry in atlasEntries where entry.width > 0 && entry.height > 0 {
                let dx = max(0, max(entry.x - center.x, center.x - entry.x - entry.width))
                let dy = max(0, max(entry.y - center.y, center.y - entry.y - entry.height))
                let candidate = hypot(dx, dy)
                if candidate < distance {
                    nearest = entry; distance = candidate
                    if candidate == 0 { break }
                }
            }
            if let entry = nearest,
               let position = camera.presentationAnchor(relativeTo: CGRect(x: entry.x, y: entry.y,
                    width: entry.width, height: entry.height), revision: atlasRevision) {
                anchorPath = entry.rawPath; anchor = position
            }
        }
        return try? AtlasFolderDestination(directory: atlasDirectory, fileScope: scope,
            customExtensions: extensions, anchorRawPath: anchorPath, cameraAnchor: anchor)
    }

    private func resolvedAtlasDestination(_ destination: AtlasFolderDestination,
        entries: [AtlasProjectCatalog.Entry]) -> AtlasFolderDestination {
        guard let path = destination.anchorRawPath,
              !entries.contains(where: { $0.rawPath == path && $0.width > 0 && $0.height > 0 }) else {
            return destination
        }
        // The folder can still be visited when its old parcel is absent. A
        // current pathname is only a map locator, never an exact source anchor.
        return (try? AtlasFolderDestination(directory: destination.directory, fileScope: destination.fileScope,
            customExtensions: destination.customExtensions)) ?? destination
    }

    @discardableResult
    private func restoreAtlasPresentation(_ destination: AtlasFolderDestination) -> Bool {
        if let path = destination.anchorRawPath, let anchor = destination.cameraAnchor,
           let entry = atlasEntries.first(where: { $0.rawPath == path }) {
            if camera.restorePresentationAnchor(anchor, relativeTo: CGRect(x: entry.x, y: entry.y,
                width: entry.width, height: entry.height), revision: atlasRevision) { return true }
        }
        return camera.restorePresentationFit(revision: atlasRevision)
    }

    private func navigateAtlasFolder(back: Bool, project: UUID, revision: UUID) {
        guard project == loadGeneration, revision == atlasRevision, canChangeAtlasDirectory else { return }
        let origin = currentAtlasDestination()
        guard let move = back ? atlasFolderNavigation.back(from: origin)
                              : atlasFolderNavigation.forward(from: origin) else { return }
        requestAtlasDestination(move.target, project: project, move: move)
    }

    private func beginAtlasLandmark(project: UUID, revision: UUID) {
        guard project == loadGeneration, revision == atlasRevision, canChangeAtlasDirectory,
              hasMetadataAtlas, !hasRetainedAtlas, !showsCity,
              let destination = currentAtlasDestination() else { return }
        atlasLandmarkDraft = (projectViewIdentity, destination)
        atlasLandmarkName = destination.directory.display
        showsAtlasLandmarkName = true
    }

    private func saveAtlasLandmark() {
        defer { atlasLandmarkDraft = nil }
        guard let draft = atlasLandmarkDraft, draft.window == projectViewIdentity,
              projectViewActive, projectOpening.canBrowse else { return }
        do {
            _ = try atlasFolderNavigation.addLandmark(label: atlasLandmarkName, destination: draft.destination)
            status = "Atlas landmark saved in this window. Use Landmarks to return to its folder and map position."
        } catch {
            status = "The landmark was not saved. Use a short, nonempty name without control characters; this window can keep 32 landmarks."
        }
    }

    private func openAtlasLandmark(_ id: UUID, project: UUID, revision: UUID) {
        guard project == loadGeneration, revision == atlasRevision, canChangeAtlasDirectory,
              let landmark = atlasFolderNavigation.landmarks.first(where: { $0.id == id }) else { return }
        let move = atlasFolderNavigation.visit(landmark.destination, from: currentAtlasDestination())
        requestAtlasDestination(landmark.destination, project: project, move: move)
    }

    @ViewBuilder private var atlasDirectoryControls: some View {
        if projectOpening.canBrowse {
            let project = loadGeneration, revision = atlasRevision
            let catalogRevision = projectOpening.catalogRevision, directory = atlasDirectory
            HStack(spacing: 10) {
                Button {
                    navigateAtlasFolder(back: true, project: project, revision: revision)
                } label: { Label("Folder Back", systemImage: "chevron.backward") }
                    .labelStyle(.iconOnly)
                    .disabled(!canChangeAtlasDirectory || !atlasFolderNavigation.canBack)
                    .keyboardShortcut("[", modifiers: [.command, .shift])
                    .help("Return to the previous folder, file filter and map position (⇧⌘[)")
                Button {
                    navigateAtlasFolder(back: false, project: project, revision: revision)
                } label: { Label("Folder Forward", systemImage: "chevron.forward") }
                    .labelStyle(.iconOnly)
                    .disabled(!canChangeAtlasDirectory || !atlasFolderNavigation.canForward)
                    .keyboardShortcut("]", modifiers: [.command, .shift])
                    .help("Return to the next folder, file filter and map position (⇧⌘])")
                VStack(alignment: .leading, spacing: 2) {
                    Text("Atlas folder").font(.caption).foregroundStyle(.secondary)
                    Text(verbatim: directory.display).font(.callout.weight(.semibold))
                        .lineLimit(2).textSelection(.enabled)
                }.frame(maxWidth: .infinity, alignment: .leading)
                Menu {
                    Button("Save current view…") { beginAtlasLandmark(project: project, revision: revision) }
                        .disabled(!canChangeAtlasDirectory || !hasMetadataAtlas || hasRetainedAtlas || showsCity
                            || atlasFolderNavigation.landmarks.count >= 32)
                    Text("Landmarks in this window")
                    ForEach(atlasFolderNavigation.landmarks) { landmark in
                        Button(landmark.label) { openAtlasLandmark(landmark.id, project: project, revision: revision) }
                            .disabled(!canChangeAtlasDirectory)
                    }
                    if !atlasFolderNavigation.landmarks.isEmpty {
                        Divider()
                        Menu("Remove landmark") {
                            ForEach(atlasFolderNavigation.landmarks) { landmark in
                                Button(landmark.label) {
                                    guard project == loadGeneration, revision == atlasRevision else { return }
                                    _ = atlasFolderNavigation.removeLandmark(id: landmark.id)
                                }
                            }
                        }
                    }
                } label: { Label("Landmarks", systemImage: "mappin.and.ellipse") }
                    .help("Save and revisit up to 32 named atlas locations in this project window")
                if let parent = directory.parent {
                    Button("Up one folder") {
                        guard atlasRevision == revision, atlasDirectory == directory else { return }
                        requestAtlasDirectory(parent, project: project, catalogRevision: catalogRevision)
                    }.disabled(!canChangeAtlasDirectory)
                    Button("Whole project") {
                        guard atlasRevision == revision, atlasDirectory == directory else { return }
                        requestAtlasDirectory(.project, project: project, catalogRevision: catalogRevision)
                    }.disabled(!canChangeAtlasDirectory)
                }
                if let selected = selectedAtlasDirectory, selected != directory {
                    Button("Focus selected folder") {
                        guard atlasRevision == revision, selectedAtlasDirectory == selected else { return }
                        requestAtlasDirectory(selected, project: project, catalogRevision: catalogRevision)
                    }.disabled(!canChangeAtlasDirectory)
                }
                if let target = atlasDirectoryTarget {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("Preparing folder atlas")
                    Text(verbatim: target.display).font(.caption).lineLimit(1)
                    Button("Cancel focus", action: cancelAtlasDirectory)
                }
            }.padding(10).background(.bar)
        }
    }

    /// Only this window's current browser may acknowledge a complete scan.
    /// First pages and paused/partial discovery cannot clear change notices.
    private var projectChangeDiscoveryKey: String {
        guard let info = projectBrowser.info else { return browserProject.uuidString }
        return "\(browserProject):\(info.owner):\(info.latest):\(info.complete):\(info.more):\(info.omissionCount)"
    }

    /// Both admission and publication protect transient current-project
    /// consumers. The manual refresh transaction closes those surfaces; an
    /// unattended refresh must wait instead of dismissing an active workflow.
    private var liveRefreshContextReady: Bool {
        guard projectViewActive, !loadingProject, !projectOpening.isPreparing,
              !atlasLayoutPending, projectOpening.canBrowse, !projectBrowser.closed, !projectBrowser.busy,
              browserProject == loadGeneration, projectBrowser.root.utf8.elementsEqual(root.utf8),
              (try? fileScope.searchScope(custom: customExtensions)) != nil,
              observedReader == nil, workspaceStreamSearch == nil, workspaceStructureSearch == nil, projectLinks == nil,
              !snapshotExport.busy, !showsFileFinder, !showsReadingDesk,
              !showsSavedRepository, !showsSnapshotComparison,
              !nativeOpen.isLoading, !nativeOpen.showsReader,
              sourceTask == nil, !searchPending, !searchFocused,
              !camera.isDragging, !camera.isAnimating else { return false }
#if FCB_APP_STORE
        guard !rootAccessLoader.isLoading, let access = rootAccess,
              access.url.path.utf8.elementsEqual(root.utf8),
              projectBrowser.rootAccessIdentity == ObjectIdentifier(access) else { return false }
#endif
        return true
    }

    private var liveRefreshInput: AtlasProjectLiveRefreshInput {
        AtlasProjectLiveRefreshInput(generation: projectChanges.generation,
            change: projectChanges.refreshTicket?.change, enabled: liveRefreshEnabled,
            watching: projectChanges.allowsAutomaticRefresh,
            canRefresh: liveRefreshContextReady && !projectRefresh.isRefreshing)
    }

    @ViewBuilder private var projectChangeNotice: some View {
        if let notice = projectChanges.notice {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "exclamationmark.arrow.triangle.2.circlepath")
                    .foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 3) {
                    Text(notice).font(.callout)
                    Text("Pinned and kept captures remain unchanged.")
                        .font(.caption).foregroundStyle(.secondary)
                    if let liveNotice = liveRefresh.notice {
                        Text(liveNotice).font(.caption).foregroundStyle(.secondary)
                    }
                }.frame(maxWidth: .infinity, alignment: .leading)
                Button("Refresh project", action: requestAtlasLoad)
                    .disabled(loadingProject || projectRefresh.isRefreshing)
                if projectChanges.needsFolderSelection {
                    Button("Choose folder…", action: chooseProject)
                }
            }.padding(10).background(.bar)
        }
    }

    @ViewBuilder private var projectRefreshNotice: some View {
        if refreshOriginProject == loadGeneration, !projectRefresh.notice.isEmpty {
            HStack(alignment: .top, spacing: 10) {
                if projectRefresh.isRefreshing {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("Preparing the refreshed project and atlas")
                }
                Text(projectRefresh.notice).font(.callout)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if projectRefresh.isRefreshing {
                    if automaticRefreshProject != nil {
                        Button("Pause live refresh") { liveRefreshEnabled = false; projectRefresh.cancel() }
                    } else {
                        Button("Cancel refresh") { projectRefresh.cancel() }
                    }
                } else {
                    Button("Dismiss") { refreshOriginProject = nil }
                }
            }.padding(10).background(.bar)
        }
    }

    @ViewBuilder private var atlasRefreshNotice: some View {
        if hasMetadataAtlas, !atlasRefreshSummary.isEmpty {
            HStack(alignment: .top, spacing: 10) {
                Text(atlasRefreshSummary).font(.callout)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if let bounds = atlasAddedBounds {
                    Button("Show added files") {
                        showsCatalog = false
                        if showsCity {
                            addedFilesFocusRevision = atlasRevision
                            showsCity = false
                        } else {
                            _ = camera.navigateFocus(bounds: bounds, revision: atlasRevision)
                        }
                    }
                }
                Button("Dismiss") { atlasRefreshSummary = "" }
            }.padding(10).background(.bar)
        }
    }

    @ViewBuilder private var retainedAtlasNotice: some View {
        if hasRetainedAtlas {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "clock.arrow.circlepath").foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 3) {
                    Text("Earlier atlas retained").font(.callout.weight(.semibold))
                    Text("Project Files uses the refreshed discovery. This map keeps its earlier membership, source previews and navigation until you rebuild it.")
                        .font(.caption).foregroundStyle(.secondary)
                    if projectOpening.isPreparing {
                        Text(loadProgress).font(.caption).foregroundStyle(.secondary)
                    }
                }.frame(maxWidth: .infinity, alignment: .leading)
                if !showsCatalog { Button("Project Files") { showsCatalog = true } }
                if projectOpening.isPreparing {
                    ProgressView().controlSize(.small)
                    Button("Stop rebuild", action: cancelAtlasLoad)
                } else {
                    Button("Rebuild atlas", action: requestAtlasPreviews)
                        .disabled(!projectOpening.canPrepare || projectRefresh.isRefreshing)
                }
            }.padding(10).background(.bar)
        }
    }

    @ViewBuilder private var rootAccessProgress: some View {
#if FCB_APP_STORE
        if rootAccessLoader.isLoading {
            HStack(spacing: 10) {
                ProgressView().controlSize(.small)
                Text("Opening project access…").font(.callout)
                Spacer()
                Button("Cancel") {
                    rootAccessLoader.cancel()
                    status = "Project access request canceled. Choose a folder when you are ready."
                }
                Button("Choose another folder…", action: chooseProject)
            }.padding(10).background(.bar)
        }
#else
        EmptyView()
#endif
    }

    private var atlasScreen: some View {
        HStack(spacing: 0) {
            VStack(spacing: 0) {
                rootAccessProgress
                projectChangeNotice
                projectRefreshNotice
                atlasDirectoryControls
                atlasRefreshNotice
                retainedAtlasNotice
                if (showsCatalog || !hasMetadataAtlas), !projectBrowser.root.isEmpty {
                    progressiveProjectView
                } else if !hasMetadataAtlas {
                    if loadingProject {
                        VStack(spacing: 16) {
                            ProgressView("Opening \((root as NSString).lastPathComponent)…")
                            Text(loadProgress)
                                .font(.callout)
                                .foregroundStyle(.secondary)
                            Button("Cancel Loading", action: cancelAtlasLoad)
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                    } else {
                        ContentUnavailableView {
                            Label("No atlas", systemImage: "map")
                        } description: {
                            Text(status)
                        } actions: {
                            Button("Choose Project Folder…", action: chooseProject)
                                .buttonStyle(.borderedProminent)
                        }
                        .frame(maxHeight: .infinity)
                    }
                } else {
                    let acceptedLayout = atlasRevision
                    ZStack {
                        AtlasCanvas(
                            active: !showsCity,
                            preservesNavigation: atlasPreservesNavigation,
                            revision: atlasRevision,
                            contentRevision: atlasResidency.contentRevision,
                            focusRequest: focusRequest,
                            selectedMatch: overlayIsCurrent ? selectedMatch : nil,
                            searchRows: overlayIsCurrent ? searchRows : [],
                            scopeLabel: atlasScopeLabel,
                            files: displayedFiles,
                            metadata: atlasEntries,
                            camera: camera,
                            hitPaths: overlayIsCurrent ? searchPaths : [],
                            selectedPath: selectedPath,
                            contentBounds: contentBounds,
                            onSelect: { entry in
                                openAtlasEntry(entry, layout: acceptedLayout)
                            },
                            onViewport: updateAtlasViewport,
                            residencySummary: hasRetainedAtlas
                                ? "Earlier source previews retained. Rebuild atlas to load refreshed previews."
                                : (atlasLayoutPending ? "Preparing the selected file scope…" : atlasResidency.summary),
                            retryPreviews: {
                                if hasRetainedAtlas { requestAtlasPreviews() }
                                else { atlasResidency.retryUnavailable() }
                            },
                            tiles: textTiles
                        )
                        .id(atlasRevision)
                        .opacity(showsCity ? 0 : 1)
                        .allowsHitTesting(!showsCity).disabled(showsCity).accessibilityHidden(showsCity)
                        if showsCity {
                            AtlasCityView(revision: atlasRevision, entries: atlasEntries,
                                complete: !hasRetainedAtlas && (projectOpening.catalog?.discoveryComplete ?? false),
                                seed: AtlasCitySeed(scale: camera.scale, offsetX: camera.offsetX, offsetY: camera.offsetY,
                                    retainedCamera: cityRefreshCamera),
                                selectedPath: selectedPath, scopeLabel: atlasScopeLabel,
                                onSelect: { openAtlasEntry($0, layout: acceptedLayout) },
                                returnTo2D: { showsCity = false },
                                onPresentedCamera: { value in
                                    guard showsCity, atlasRevision == acceptedLayout, cityPresentedCamera != value else { return }
                                    cityPresentedCamera = value
                                })
                                .id(atlasRevision)
                        }
                    }
                }
                if (hasSourcePane && showsReader) || !pinnedReaders.entries.isEmpty || pinnedReaders.busy {
                    Divider()
                    HSplitView {
                        if hasSourcePane && showsReader {
                            sourcePane.frame(minWidth: 320)
                        }
                        if !pinnedReaders.entries.isEmpty || pinnedReaders.busy {
                            AtlasPinnedReadersView(readers: pinnedReaders,
                                canKeep: readingDesk.canOperate, keep: keepSource,
                                project: projectBrowser, searchProject: selectedSourceProjectSearch).frame(minWidth: 320)
                        }
                    }
                    .frame(height: (!pinnedReaders.entries.isEmpty || pinnedReaders.busy) ? 380 : (usesPagedReader ? 320 : 240))
                }
            }
            .frame(maxWidth: .infinity)

            if selectedPath != nil && showsReader && pinnedReaders.entries.isEmpty && !pinnedReaders.busy {
                Divider()
                inspector.frame(width: 264)
            }
        }
    }

    private func openAtlasEntry(_ entry: AtlasProjectCatalog.Entry, layout: UUID) {
        guard layout == atlasRevision, hasMetadataAtlas, !atlasLayoutPending else { return }
        guard let path = entry.sourcePath, fileScope.includes(path, custom: customExtensions) else {
            status = "This filename is available through Project Files or Quick Open."
            return
        }
        selectedHit = nil; openFile(path); showsReader = true
    }

    private func catalogView(_ catalog: AtlasProjectCatalog) -> some View {
        let project = loadGeneration, catalogRevision = projectOpening.catalogRevision
        return AtlasProjectCatalogView(catalog: catalog, entries: catalogEntries,
            preparing: projectOpening.isPreparing, canPrepare: projectOpening.canPrepare,
            hasAtlas: hasMetadataAtlas, progress: loadProgress,
            build: requestAtlasPreviews, cancel: cancelAtlasLoad, refresh: requestAtlasLoad,
            showAtlas: { showsCatalog = false }) { id in
                openCatalogFile(id, project: project, catalogRevision: catalogRevision)
            }
            .id(project)
    }

    private var progressiveProjectView: some View {
        let browser = projectBrowser, project = loadGeneration, rangeAuthority = snapshotExportSource
        let visible = projectBrowser.page?.files.filter { file in
            String(bytes: file.rawPath, encoding: .utf8).map { fileScope.includes($0, custom: customExtensions) }
                ?? (fileScope == .all)
        } ?? []
        return AtlasProjectBrowserView(model: projectBrowser, entries: visible,
            scopeLabel: fileScope == .custom ? customExtensions : fileScope.rawValue,
            preparing: projectOpening.isPreparing, canPrepare: projectOpening.canPrepare,
            // A cold atlas has no source tiles until its first visible frame
            // requests them. Metadata must make Show text atlas available.
            hasAtlas: hasMetadataAtlas, progress: loadProgress,
            build: requestAtlasPreviews, cancelPreviews: cancelAtlasLoad, refresh: requestAtlasLoad,
            showAtlas: { showsCatalog = false }, canKeep: readingDesk.canOperate, keep: { reader in
                // Keep the capture without replacing the file reader's sheet.
                // The Reading Desk toolbar opens the independently kept source.
                readingDesk.keep(reader: reader)
            }, canReadRanges: rangeAuthority != nil, readRanges: { file, page in
                // A queued action from an older rendered page cannot borrow a
                // replacement project's grant, even when the root text matches.
                guard let authority = rangeAuthority, projectBrowser === browser,
                      snapshotExportSource?.identity == authority.identity else { throw AtlasProjectBrowserError.stale }
                let source = try browser.observedSource(file, from: page, root: authority.root,
                    project: authority.identity.project, browserOwner: authority.identity.browserOwner,
                    accessLease: authority.access)
                closeObservedReader()
                observedReader = AtlasObservedReaderPresentation(source: source, transport: .native)
            }, searchProject: selectedSourceProjectSearch,
            canChangeDiscoveryPolicy: !projectRefresh.isRefreshing && !loadingProject,
            changeDiscoveryPolicy: { policy in
                guard projectBrowser === browser else { return }
                _ = requestProjectRefresh(discoveryPolicy: policy)
            }, canFocusDirectory: canChangeAtlasDirectory, focusDirectory: { file, page in
                guard projectBrowser === browser, loadGeneration == project, canChangeAtlasDirectory else {
                    throw AtlasProjectBrowserError.stale
                }
                guard let accepted = browser.currentFile(file, from: page) else { throw AtlasProjectBrowserError.stale }
                let directory = try AtlasDirectoryFocus.containingFile(rawPath: accepted.rawPath)
                requestAtlasDirectory(directory, project: project)
            })
            .id(projectViewIdentity)
    }

    private func openCatalogFile(_ id: UInt64, project: UUID, catalogRevision: UUID) {
        guard !loadingProject, let entry = projectOpening.file(id, in: project, catalogRevision: catalogRevision),
              let path = entry.sourcePath, fileScope.includes(path, custom: customExtensions) else { return }
#if FCB_APP_STORE
        guard rootAccess?.url.path.utf8.elementsEqual(root.utf8) == true else { return }
#endif
        selectedHit = nil; openFile(path); showsReader = true
    }

    private func updateCatalogFilter() {
        catalogEntries = projectOpening.catalog?.entries.filter { entry in
            entry.sourcePath.map { fileScope.includes($0, custom: customExtensions) } ?? (fileScope == .all)
        } ?? []
    }

    private var inspector: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Inspector")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Color.accentColor)
            if let path = selectedPath, let file = files.first(where: { $0.path.utf8.elementsEqual(path.utf8) }) {
                Text(file.fileName + " · file")
                    .font(.system(size: 13, weight: .medium))
                Text(path)
                    .font(.system(size: 10.5, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                Divider()
                if atlasUnconfirmedPaths.contains(Array(path.utf8)) {
                    stat("earlier observed bytes", ByteCountFormatter.string(fromByteCount: Int64(file.bytes), countStyle: .file))
                    Text("Not re-observed in the partial refresh. This parcel does not prove the file is still present.")
                        .font(.caption).foregroundStyle(.orange)
                } else if !hasRetainedAtlas, let entry = projectOpening.catalog?.entries.first(where: { $0.rawPath.elementsEqual(path.utf8) }) {
                    stat("observed bytes", String(entry.observedBytes))
                } else {
                    stat(hasRetainedAtlas ? "earlier observed bytes" : "bytes",
                        ByteCountFormatter.string(fromByteCount: Int64(file.bytes), countStyle: .file))
                }
                stat("profile rows", "\(file.lineCount)")
                stat("preview coverage", file.previewCoverage)
                stat("captured lines", selectedSource.map { "\($0.lines.count)" } ?? "—")
                let fileHits = hits.filter { $0.sourcePath == path }
                stat("matches", "\(fileHits.count)")
                stat(hasRetainedAtlas ? "earlier atlas files" : "project files", "\(files.count)")
            } else {
                Text("Click a file cell to inspect it.")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                stat(hasRetainedAtlas ? "earlier atlas files" : "project files", "\(files.count)")
                stat("project", (root as NSString).lastPathComponent)
            }
            Divider()
            VStack(alignment: .leading, spacing: 7) {
                Text("Relationships").font(.caption.weight(.semibold))
                if selectedPath != nil || workspaceReader != nil {
                    Button { openProjectLinks(.outgoing, projection: .imports) } label: {
                        Label("Imports from this file", systemImage: "arrow.up.right")
                    }
                    Button { openProjectLinks(.incoming, projection: .imports) } label: {
                        Label("Files importing this target", systemImage: "arrow.turn.up.left")
                    }
                    Button { openProjectLinks(.incoming) } label: {
                        Label("Documentation backlinks", systemImage: "arrow.turn.up.left")
                    }
                    Button { openProjectLinks(.outgoing) } label: {
                        Label("Documentation links", systemImage: "arrow.up.right")
                    }
                }
                Button { openProjectLinks() } label: {
                    Label("Browse project relationships", systemImage: "link")
                }
                Text("Read exact import statements or captured Markdown links. Import targets are catalog candidates, not compiler-resolved dependencies.")
                    .font(.caption2).foregroundStyle(.secondary)
            }
            .controlSize(.small).disabled(snapshotExportSource == nil)
            Spacer()
        }
        .padding(12)
        .frame(maxHeight: .infinity, alignment: .topLeading)
        .background(.bar)
    }

    private func stat(_ label: String, _ value: String) -> some View {
        HStack {
            Text(label).font(.system(size: 11)).foregroundStyle(.secondary)
            Spacer()
            Text(value).font(.system(size: 11, design: .monospaced))
        }
    }

    private var sourcePane: some View {
        VStack(spacing: 0) {
            if let label = workspaceReader?.label ?? selectedPath ?? readerHistory.requestedLabel {
                HStack(spacing: 8) {
                    Image(systemName: "doc.text")
                        .foregroundStyle(.secondary)
                    Text(verbatim: label)
                        .font(.system(size: 11.5, design: .monospaced))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    if let source = selectedObservedSource {
                        Button("Read current ranges") {
                            guard selectedObservedSource?.identity == source.identity else { return }
                            closeObservedReader()
                            observedReader = AtlasObservedReaderPresentation(source: source, transport: .native)
                        }
                            .help("Explicitly open the current project file in bounded observed ranges, including files larger than 32 MiB. Keep this retained source unchanged.")
                    }
                    if workspaceReader != nil {
                        Text("Retained source capture").font(.caption).foregroundStyle(.secondary)
                    } else if let path = selectedPath {
                        let fileHits = hits.filter { $0.sourcePath == path }
                        Text("\(fileHits.count) match\(fileHits.count == 1 ? "" : "es") in file")
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                    }
                    if !usesPagedReader, let path = selectedPath {
                        if selectedSource != nil, sourceError == nil {
                            Menu("Display") {
                                Toggle("Line numbers", isOn: $showsSourceLineNumbers)
                                Toggle("Visible whitespace", isOn: $showsSourceWhitespace)
                                Toggle("Wrap lines", isOn: $wrapsSourceLines)
                            }
                            .menuStyle(.borderlessButton).fixedSize().controlSize(.small)
                            .help("Show physical source lines and whitespace, or wrap long lines. These display choices preserve source text and selection.")
                        }
                        Button("Read pages") { openPagedFile(path) }
                            .help(selectedSource == nil ? "Capture the current file for byte and line navigation"
                                : "Read the already captured bytes with paging, Find and Outline. Refresh from disk explicitly to load edits.")
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(.bar)
                Divider()
            }
            if retainedReaderPresentation?.navigation == focusRequest {
                Text("Project refresh keeps this reader independent. Read current ranges opens the file as it is now.")
                    .font(.caption).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 12).padding(.vertical, 4)
            }
            if readerHistory.current != nil || readerHistory.busy {
                HStack(spacing: 8) {
                    Button { sourceBack() } label: { Label("Source Back", systemImage: "chevron.left") }
                        .disabled(readerHistory.busy || (usesPagedReader ? !readerHistory.canBack : readerHistory.current == nil))
                    Button { sourceForward() } label: { Label("Source Forward", systemImage: "chevron.right") }
                        .disabled(!usesPagedReader || !readerHistory.canForward)
                    Text("\(readerHistory.position) of \(readerHistory.count) retained sources")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    if readerHistory.busy {
                        ProgressView().controlSize(.small)
                        Button("Cancel opening", action: readerHistory.cancel)
                            .disabled(readerHistory.canceling && readerHistory.requestedLabel == nil)
                    }
                }.controlSize(.small).padding(.horizontal, 12).padding(.vertical, 5)
                    .help("Keep the last three source captures and their page and Find state. Source Back and Forward never reread a file.")
            }
            if !readerHistory.notice.isEmpty {
                Text(readerHistory.notice).font(.caption).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 12).padding(.vertical, 3)
            }
            ZStack(alignment: .topLeading) {
                ForEach(readerHistory.captures) { capture in
                    workspaceSourceView(capture)
                        .opacity(workspaceReader === capture ? 1 : 0)
                        .allowsHitTesting(workspaceReader === capture)
                        .disabled(workspaceReader !== capture)
                        .accessibilityHidden(workspaceReader !== capture)
                }
                if workspaceReader == nil {
                    if let source = selectedSource, sourceError == nil {
                        AtlasSourceReader(source: source, navigation: focusRequest,
                            selection: (searchIsCurrent ? selectedReaderSelection : nil) ?? retainedReaderSelection,
                            wrapsLines: wrapsSourceLines,
                            showsLineNumbers: showsSourceLineNumbers, showsWhitespace: showsSourceWhitespace,
                            firstPhysicalLine: 1, usesSyntaxPalette: true) {
                            if let document = atlasDocuments[source.path],
                               document.source.path.utf8.elementsEqual(source.path.utf8),
                               document.source.text.utf8.elementsEqual(source.text.utf8) {
                                return AtlasDocument.style(document.capture)
                            }
                            return NSAttributedString(string: source.text, attributes: [
                                .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular),
                                .foregroundColor: Monokai.color("plain")
                            ])
                        }
                    } else {
                        ScrollView {
                            Text(sourceError ?? fileText)
                                .font(.system(size: 12, design: .monospaced))
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .topLeading)
                                .padding(10)
                        }
                    }
                }
            }
            .background(AtlasDocumentLinkFocus(capture: workspaceReader?.id)
                .allowsHitTesting(false).accessibilityHidden(true))
        }
        .background(Color(nsColor: .textBackgroundColor))
    }

    private func workspaceSourceView(_ capture: AtlasWorkspaceReaderCapture) -> some View {
        AtlasPagedSourceView(model: capture.reader,
            canPin: workspaceReader === capture && pinnedReaders.canAdd,
            pin: { reader in
                guard workspaceReader === capture, capture.reader === reader else { return }
                pinSource(reader)
            },
            canKeep: workspaceReader === capture && readingDesk.canOperate,
            keep: { reader in
                guard workspaceReader === capture, capture.reader === reader else { return }
                keepSource(reader)
            }, project: projectBrowser, searchProject: workspaceSourceProjectSearch(for: capture),
            preferredPreviewRequest: markdownPreview.request(for: capture.id, path: capture.relativePath,
                active: workspaceReader === capture && canPresentWorkspacePreview),
            returnedToSource: {
                guard workspaceReader === capture else { return }
                markdownPreview.showSource()
            })
    }

    private func workspaceSourceProjectSearch(for capture: AtlasWorkspaceReaderCapture)
        -> ((AtlasReaderSelectionQuery) -> Void)? {
        guard let search = selectedSourceProjectSearch else { return nil }
        return { selected in
            guard workspaceReader === capture else { return }
            search(selected)
        }
    }

    // MARK: Actions

    private func toggleMarkdownPreview() {
        markdownPreview.toggle()
        guard markdownPreview.enabled else { return }
        // A window command can enrich the atlas without reopening a reader the
        // user hid. Its request remains inactive until that pane is shown again.
        if hasMetadataAtlas && !showsReader {
            status = "Markdown preview enabled in atlas parcels. Zoom in to read captured previews."
            return
        }
        if readerHistory.busy {
            status = "Markdown preview enabled. The selected source will open after its capture is ready."
            return
        }
        if let capture = workspaceReader {
            markdownPreview.activate(capture.id)
            showsReader = true
        } else if let path = selectedPath, AtlasMarkdownPreviewPreference.isMarkdown(Array(path.utf8)) {
            // The existing handoff retains displayed bytes when available;
            // enabling preview never reopens an already accepted reader.
            openPagedFile(path)
        } else {
            status = "Markdown preview enabled in atlas parcels. Open a Markdown file to read or navigate its retained preview."
        }
    }

    private func keepSource(_ reader: AtlasPagedReaderModel) {
        guard readingDesk.canOperate, reader.canKeep,
              let domain = readingDesk.readerDomain, reader.retainedRegistryDomain == domain else { return }
        // The desk owns its imported capture independently of project changes.
        // Its receiving operation keeps the source reader alive until delivery.
        readingDesk.keep(reader: reader)
        showsReadingDesk = true
    }

    private func pinSource(_ reader: AtlasPagedReaderModel) {
        guard let capture = workspaceReader, capture.reader === reader, reader.canPin,
              capture.root.utf8.elementsEqual(root.utf8) else { return }
#if FCB_APP_STORE
        guard !rootAccessLoader.isLoading, let access = rootAccess,
              access.url.path.utf8.elementsEqual(root.utf8) else { return }
#endif
        pinnedReaders.pin(reader, root: root, label: capture.label)
    }

    private func bindWorkspaceReaderHistory() {
#if FCB_APP_STORE
        guard let access = rootAccess, access.url.path.utf8.elementsEqual(root.utf8) else {
            readerHistory.close()
            return
        }
        readerHistory.bind(root: root, accessIdentity: ObjectIdentifier(access))
#else
        readerHistory.bind(root: root, accessIdentity: nil)
#endif
    }

    /// Opening is a proposal: the displayed capture and Forward entries remain
    /// usable until the new reader has validated its first page/exact target.
    private func openWorkspaceReader(_ path: String, captured: AtlasSearchCapturedHit? = nil,
                                     supplied: AtlasSuppliedReaderSource? = nil, initialLine: UInt64? = nil) {
        guard !loadingProject else { return }
        let accessLease: AnyObject?
#if FCB_APP_STORE
        guard !rootAccessLoader.isLoading, let access = rootAccess,
              access.url.path.utf8.elementsEqual(root.utf8) else {
            status = "Choose the project folder again to grant read access."
            return
        }
        accessLease = access
#else
        accessLease = nil
#endif
        cancelSourceLoad()
        // Keep explicit range-reading recovery available when the first file
        // cannot be captured completely. Never relabel an accepted reader.
        if readerHistory.current == nil, selectedSource == nil {
            selectedPath = path
        }
        if captured == nil { selectedHit = nil }
        readerHistory.open(root: root, path: path, accessLease: accessLease, initialLine: initialLine) {
            AtlasPagedReaderModel(transport: .native, search: .native, outline: .native,
                document: .native, captured: captured, supplied: supplied)
        }
        showsReader = true
        status = "Opening \(path). The retained reader stays available until the new source is ready."
    }

    private func presentWorkspaceReader() {
        guard let capture = readerHistory.current else { return }
        markdownPreview.activate(capture.id)
        cancelSourceLoad()
        selectedMatch = selectedHit.flatMap { row in
            guard searchIsCurrent,
                  let hit = searchPresentation?.captureCandidate(for: row, in: searchContext),
                  hit.sourcePath?.utf8.elementsEqual(capture.relativePath) == true else { return nil }
            return resolvedMatches[hit.id]
        }
        selectedReaderSelection = nil
        selectedSource = nil; sourceError = nil; fileText = ""
        selectedPath = capture.path
        usesPagedReader = true
        showsReader = true
        focusRequest = UUID()
        status = capture.reader.notice.isEmpty ? readerHistory.notice : capture.reader.notice
    }
    private func sourceBack() {
        selectedHit = nil
        if usesPagedReader { readerHistory.back() }
        presentWorkspaceReader()
    }
    private func sourceForward() {
        guard usesPagedReader else { return }
        selectedHit = nil
        readerHistory.forward()
        presentWorkspaceReader()
    }

    /// Revoke old camera coordinates when publishing a layout, before a
    /// queued action from the previous SwiftUI canvas can run.
    private func renewAtlasRevision() {
        invalidateVisibleSearchScope()
        atlasPreservesNavigation = false
        cityRefreshCamera = nil; cityPresentedCamera = nil; addedFilesFocusRevision = nil
        let revision = UUID()
        camera.bindNavigation(to: revision)
        atlasRevision = revision
    }

    /// A same-root refresh prepares a private first page while the accepted
    /// project remains usable. Root/grant replacements use the separate initial
    /// load route and immediately revoke their previous source authority.
    private func requestAtlasLoad() {
        if projectOpening.canBrowse, !projectBrowser.closed,
           browserProject == loadGeneration, projectBrowser.root.utf8.elementsEqual(root.utf8) {
            requestProjectRefresh()
        } else {
            replaceProject()
        }
    }

    @discardableResult private func requestProjectRefresh(automatic: AtlasProjectChangeTicket? = nil,
        discoveryPolicy: AtlasDiscoveryPolicy? = nil) -> Bool {
        guard projectViewActive, !projectRefresh.isRefreshing, !loadingProject,
              !projectOpening.isPreparing, !atlasLayoutPending,
              projectOpening.canBrowse, !projectBrowser.closed,
              browserProject == loadGeneration, projectBrowser.root.utf8.elementsEqual(root.utf8) else { return false }
        if let automatic {
            guard liveRefreshInput.admits(automatic), projectChanges.acceptsAutomaticRefresh(automatic) else { return false }
        }
        let previous = projectBrowser, previousProject = browserProject
        let requestedDiscoveryPolicy = discoveryPolicy ?? previous.discoveryPolicy
        let retainingPreviousMembership = requestedDiscoveryPolicy == previous.discoveryPolicy
        let requestedRoot = root, requestedDirectory = atlasDirectory
        let previousRevision = atlasRevision, previousRequest = atlasLayoutRequest
        let requestedScope = fileScope, requestedExtensions = Array(customExtensions.utf8)
        guard let scope = try? fileScope.searchScope(custom: customExtensions) else {
            status = "Choose a valid file scope before refreshing the atlas."
            return false
        }
        let includesUnsupportedNames = fileScope == .all
        let previousEntries = atlasEntries, previousBounds = contentBounds, refreshAtlas = hasMetadataAtlas
        var stagedAtlas: AtlasPreparedProjectRefresh?
        let accessLease: AnyObject?
#if FCB_APP_STORE
        guard !rootAccessLoader.isLoading, let access = rootAccess,
              access.url.path.utf8.elementsEqual(root.utf8) else {
            status = "Choose the project folder again to grant read access."
            return false
        }
        accessLease = access
#else
        accessLease = nil
#endif
        if let project = projectRefresh.stage(root: root, project: previousProject, accessLease: accessLease,
            discoveryPolicy: requestedDiscoveryPolicy,
            prepare: { cancellation in
                guard refreshAtlas else { return }
                stagedAtlas = try await projectRefreshIO.perform(cancellation: cancellation,
                    keepingAlive: accessLease.map { [$0] } ?? []) {
                    try AtlasPreparedProjectRefresh.prepare(root: requestedRoot, directory: requestedDirectory,
                        discoveryPolicy: requestedDiscoveryPolicy,
                        previous: previousEntries, previousBounds: previousBounds,
                        retainingPreviousMembership: retainingPreviousMembership, scope: scope,
                        includesUnsupportedNames: includesUnsupportedNames, cancellation: cancellation)
                }
            },
            accepted: { candidate in
                if let automatic {
                    guard liveRefreshEnabled, liveRefreshContextReady,
                          projectChanges.acceptsAutomaticRefresh(automatic) else { return false }
                }
                guard atlasRevision == previousRevision, atlasLayoutRequest == previousRequest,
                      atlasDirectory == requestedDirectory,
                      fileScope == requestedScope, Array(customExtensions.utf8) == requestedExtensions,
                      hasMetadataAtlas == refreshAtlas, !atlasLayoutPending, !projectOpening.isPreparing,
                      candidate.browser.discoveryPolicy == requestedDiscoveryPolicy else { return false }
                return acceptProjectRefresh(candidate, replacing: previous, atlas: stagedAtlas, accessLease: accessLease)
            },
            failed: { project in projectChanges.cancelDiscovery(project: project) }) {
            automaticRefreshProject = automatic == nil ? nil : project
            refreshOriginProject = previousProject
            // The coordinator opens on a later main-actor turn. Record the
            // watcher epoch before any candidate result can be delivered.
            projectChanges.bind(root: root, accessLease: accessLease)
            liveRefresh.didStart(projectChanges.refreshTicket, input: liveRefreshInput)
            projectChanges.beginDiscovery(project: project)
            return true
        }
        return false
    }

    /// Commit only after the replacement page is validated. There is no await
    /// between checking the owning project/grant and revoking its old actions.
    /// Failed/canceled preparation never enters this path.
    private func acceptProjectRefresh(_ candidate: AtlasProjectRefresh.Candidate,
        replacing previous: AtlasProjectBrowserModel, atlas: AtlasPreparedProjectRefresh?, accessLease: AnyObject?) -> Bool {
        guard projectViewActive, projectBrowser === previous, !previous.closed,
              browserProject == candidate.previousProject, loadGeneration == candidate.previousProject,
              root.utf8.elementsEqual(candidate.root.utf8),
              candidate.browser.root.utf8.elementsEqual(candidate.root.utf8),
              candidate.project != candidate.previousProject, candidate.opening.projectID == candidate.project,
              candidate.opening.canBrowse, candidate.browser.currentPage, !candidate.browser.closed,
              let info = candidate.browser.info else { return false }
#if FCB_APP_STORE
        guard !rootAccessLoader.isLoading,
              rootAccess.map(ObjectIdentifier.init) == candidate.accessIdentity,
              rootAccess?.url.path.utf8.elementsEqual(candidate.root.utf8) == true else { return false }
#else
        guard candidate.accessIdentity == nil else { return false }
#endif
        let preservesFolderSession = previous.discoveryPolicy == candidate.browser.discoveryPolicy
        var replacementOpening = candidate.opening
        if let atlas {
            guard atlas.layout.root.utf8.elementsEqual(candidate.root.utf8),
                  atlas.catalog.requestedRoot?.utf8.elementsEqual(candidate.root.utf8) == true,
                  atlas.catalog.requestedDirectory == atlasDirectory,
                  atlas.catalog.matchesDiscoveryPolicy(candidate.browser.discoveryPolicy.wireName),
                  replacementOpening.acceptPreparedAtlas(atlas.catalog, in: candidate.project,
                    hasOpenableFiles: !atlas.files.isEmpty),
                  atlasResidency.install(atlas.layout, accessLease: accessLease,
                    sourceCacheHandle: projectCache?.sourceHandle ?? 0, cacheLease: projectCache) else { return false }
        }
        snapshotExport.cancel()
        closeObservedReader()
        closeWorkspaceStreamSearch()
        closeWorkspaceStructureSearch()
        closeProjectLinks()
        retainedAtlasRevision = nil
        readerHistory.catalogRefreshed()
        if showsReader, hasSourcePane {
            retainedReaderPresentation = (focusRequest, selectedReaderSelection ?? retainedReaderSelection)
        }
        // These inputs and focusRequest own the mounted paged reader. Keeping
        // them preserves its capture, page, Find query and native selection.
        // A pending legacy pathname read has no retained capture to transfer.
        if sourceTask != nil, selectedSource == nil, !usesPagedReader {
            sourceError = "Source opening stopped when the project refreshed. Open the file again from Project Files."
        }
        // Detach an accepted reader from its old workspace row before clearing
        // visible-scope/search authority, which otherwise dismisses row opens.
        cancelSourceLoad()
        suspendAtlasPreparation(suspendingResidency: atlas == nil)
        workspaceSearch?.refresh()
        showsFileFinder = false
        clearSearch()
        selectedMatch = nil
        loadTask?.cancel()
        loadCancellation?.cancel()
        projectIO.cancel()
        loadTask = nil
        loadCancellation = nil
        loadProgress = ""
        projectOpening = replacementOpening
        browserProject = candidate.project
        projectBrowser = candidate.browser
        previous.close()
        refreshOriginProject = candidate.project
        if let atlas {
            cityRefreshCamera = showsCity ? cityPresentedCamera : nil
            atlasEntries = atlas.reconciliation.entries
            atlasUnconfirmedPaths = atlas.reconciliation.unconfirmedRawPaths
            atlasRefreshSummary = atlas.notice
            atlasAddedBounds = atlas.addedBounds
            files = atlas.files; displayedFiles = atlas.files
            contentBounds = atlas.bounds; camera.contentBounds = atlas.bounds
            camera.bindNavigation(to: atlas.layout.revision, preservingHistory: preservesFolderSession)
            atlasPreservesNavigation = preservesFolderSession
            atlasRevision = atlas.layout.revision
            catalogEntries = atlas.visibleCatalog
        } else {
            showsCatalog = true
            showsCity = false
            files = []
            displayedFiles = []
            catalogEntries = []
        }
        if !preservesFolderSession { resetAtlasFolderNavigation() }
        atlasRestoredScope = nil
        if let destination = currentAtlasDestination(usingCurrentFilter: true) {
            atlasFolderNavigation.rebaseCurrent(destination)
            if atlas != nil {
                atlasPreservesNavigation = restoreAtlasPresentation(destination)
            }
        }
        // A first page can still have unvisited members or omissions. Only a
        // real complete scan under the same uninterrupted watcher clears hints.
        projectChanges.reportDiscovery(project: candidate.project, complete: info.complete,
            more: info.more, omissions: info.omissionCount)
        status = atlas?.notice ?? candidate.browser.notice
        if !query.isEmpty { runSearch(debounce: true) }
        return true
    }

    /// Initial project opening performs only the bounded metadata operation.
    /// Text capture/highlighting/raster preparation is a subsequent action.
    private func replaceProject() {
        liveRefresh.close()
        automaticRefreshProject = nil
        projectRefresh.cancel()
        refreshOriginProject = nil
        projectViewIdentity = UUID()
        resetAtlasFolderNavigation()
        retainedReaderPresentation = nil
        snapshotExport.cancel()
        closeObservedReader()
        closeWorkspaceStreamSearch()
        closeWorkspaceStructureSearch()
        closeProjectLinks()
        closeAtlasResidency()
        readerHistory.close()
        workspaceSearch?.refresh()
        projectBrowser.close()
        projectBrowser = AtlasProjectBrowserModel(transport: .native)
        showsFileFinder = false
        cancelSourceLoad()
        clearSearch()
        selectedPath = nil
        selectedSource = nil
        selectedMatch = nil
        sourceError = nil
        fileText = ""
        loadTask?.cancel()
        loadCancellation?.cancel()
        projectIO.cancel()
        let ticket = projectOpening.begin()
        browserProject = ticket.project
        searchPathKind = .workspace
        searchPathInput = ""
        let cancellation = AtlasSearchCancellation()
        loadCancellation = cancellation
        let requestedRoot = root
        let accessLease: AnyObject?
        pinnedReaders.bind(root: requestedRoot)
#if FCB_APP_STORE
        accessLease = rootAccess
        if rootAccess?.url.path.utf8.elementsEqual(requestedRoot.utf8) == true {
            projectChanges.bind(root: requestedRoot, accessLease: accessLease)
        } else {
            projectChanges.close()
        }
#else
        accessLease = nil
        projectChanges.bind(root: requestedRoot, accessLease: nil)
#endif
        projectChanges.beginDiscovery(project: ticket.project)
        showsReader = false
        showsCatalog = true
        focusRequest = UUID()
        renewAtlasRevision()
        files = []
        displayedFiles = []
        catalogEntries = []
        loadProgress = "Discovering the first file page…"
        let browser = projectBrowser
        loadTask = Task {
            await loadProgressiveProject(browser, root: requestedRoot, ticket: ticket,
                cancellation: cancellation, accessLease: accessLease)
        }
    }

    private func cancelAtlasLoad() {
        if projectOpening.isDiscovering { projectChanges.cancelDiscovery(project: browserProject) }
        loadTask?.cancel()
        loadCancellation?.cancel()
        projectIO.cancel()
        atlasLayoutRequest = UUID()
        atlasLayoutTask?.cancel(); atlasLayoutTask = nil
        atlasLayoutCancellation?.cancel(); atlasLayoutCancellation = nil
        atlasLayoutIO.cancel(); atlasLayoutPending = false
        atlasDirectoryTarget = nil
        loadTask = nil
        loadCancellation = nil
        projectOpening.cancel()
        loadProgress = ""
        status = projectOpening.canBrowse
            ? "Text-atlas preparation stopped. The catalog, search and source reader remain available."
            : "Project opening canceled. Choose a folder to try again."
    }

    private func loadProgressiveProject(_ browser: AtlasProjectBrowserModel, root requestedRoot: String, ticket: AtlasProjectOpening.Ticket,
        cancellation: AtlasSearchCancellation, accessLease: AnyObject?) async {
        guard !Task.isCancelled, !cancellation.isCanceled, projectOpening.accepts(ticket),
              root.utf8.elementsEqual(requestedRoot.utf8) else { return }
        defer {
            if projectOpening.accepts(ticket) {
                projectChanges.cancelDiscovery(project: ticket.project)
                projectOpening.fail(ticket)
                loadTask = nil; loadCancellation = nil
            }
        }
#if FCB_APP_STORE
        guard rootAccess?.url.path.utf8.elementsEqual(requestedRoot.utf8) == true else {
            status = "Choose a project folder to grant read access."
            return
        }
#endif
        do {
            try await browser.open(root: requestedRoot, accessLease: accessLease)
        } catch {
            if !cancellation.isCanceled, projectOpening.accepts(ticket) {
                status = "Could not discover this project (\(error)). Choose another folder or retry."
            }
            return
        }
        guard !Task.isCancelled, !cancellation.isCanceled, projectOpening.accepts(ticket),
              projectBrowser === browser, root.utf8.elementsEqual(requestedRoot.utf8), browser.info != nil,
              projectOpening.acceptProgressive(for: ticket) else { return }
        loadTask = nil; loadCancellation = nil; loadProgress = ""
        status = browser.notice
        if !query.isEmpty { runSearch(debounce: true) }
        // The explicit single-component filename is independent of metadata
        // page limits, discovery ignores and presentation filters. Its reader
        // captures under this established root using the normal native checks.
        if AtlasFileStartup.isPending { openStartupFile() }
    }

    private func loadPreviewCatalog(root requestedRoot: String, directory: AtlasDirectoryFocus,
        discoveryPolicy: AtlasDiscoveryPolicy,
        ticket: AtlasProjectOpening.Ticket, cancellation: AtlasSearchCancellation, accessLease: AnyObject?) async throws {
        let catalog = try await projectIO.perform(cancellation: cancellation,
            keepingAlive: accessLease.map { [$0] } ?? []) {
            try AtlasProjectWorker.catalogReport(root: requestedRoot, directory: directory, discoveryPolicy: discoveryPolicy,
                cancellation: cancellation)
        }
        guard !Task.isCancelled, !cancellation.isCanceled, projectOpening.accepts(ticket),
              browserProject == ticket.project, projectBrowser.discoveryPolicy == discoveryPolicy,
              root.utf8.elementsEqual(requestedRoot.utf8), atlasDirectory == directory,
              catalog.requestedRoot?.utf8.elementsEqual(requestedRoot.utf8) == true,
              catalog.requestedDirectory == directory,
              projectOpening.acceptPreviewCatalog(catalog, for: ticket) else { throw AtlasProjectIOError.canceled }
        // A retained map remains one coherent layout while its replacement is
        // prepared. Publish the fresh files with the new layout, never beneath
        // old parcels if preparation fails or the user cancels it.
        if !hasRetainedAtlas { updateAtlasFiles(catalog) }
        updateCatalogFilter()
    }

    private func updateAtlasFiles(_ catalog: AtlasProjectCatalog) {
        // AtlasFile's legacy byte field is presentation-only. The catalog keeps
        // full UInt64 observations, including values above native Int.max.
        files = catalog.entries.compactMap { entry in
            guard let path = entry.sourcePath else { return nil }
            return AtlasFile(path: path, x: entry.x, y: entry.y, w: entry.width, h: entry.height,
                bytes: Int(clamping: entry.observedBytes), lineCount: 0, sourceLineCount: nil,
                profileState: "disabled", profile: [], avgColor: AtlasFile.averageColor(profile: []))
        }
        displayedFiles = files.filter { fileScope.includes($0.path, custom: customExtensions) }
    }

    private func openStartupFile() {
        guard AtlasFileStartup.isPending, !loadingProject, projectOpening.canBrowse,
              projectOpening.projectID == browserProject, !projectBrowser.closed,
              projectBrowser.root.utf8.elementsEqual(root.utf8) else { return }
        let outcome = AtlasFileStartup.consume(root: root)
        if case .open(let path, let line) = outcome {
            selectedHit = nil
            openFile(path, initialLine: line)
            showsReader = true
        } else if let message = outcome.message {
            status = message
        }
    }

    private func requestAtlasPreviews() {
        guard projectOpening.canPrepare, !root.isEmpty, !atlasLayoutPending, !projectRefresh.isRefreshing else { return }
        let accessLease: AnyObject?
#if FCB_APP_STORE
        guard let access = rootAccess, access.url.path.utf8.elementsEqual(root.utf8) else {
            status = "Choose the project folder again to grant read access."
            return
        }
        accessLease = access
#else
        accessLease = nil
#endif
        guard let ticket = projectOpening.beginPreviews() else { return }
        let requestedRoot = root, requestedDirectory = atlasDirectory
        let requestedDiscoveryPolicy = projectBrowser.discoveryPolicy
        let cancellation = AtlasSearchCancellation()
        loadCancellation = cancellation
        // Publishing optional atlas tiles must not dismiss a file reader that
        // the user opened while preparation ran. Show text atlas is explicit.
        if !hasMetadataAtlas { showsCatalog = true }
        loadProgress = projectOpening.catalog == nil ? "Collecting the bounded text-atlas catalog…"
            : "Preparing the file atlas…"
        loadTask = Task {
            await prepareAtlas(root: requestedRoot, directory: requestedDirectory,
                discoveryPolicy: requestedDiscoveryPolicy, ticket: ticket,
                cancellation: cancellation, accessLease: accessLease)
        }
    }

    private func prepareAtlas(root requestedRoot: String, directory: AtlasDirectoryFocus,
        discoveryPolicy: AtlasDiscoveryPolicy,
        ticket: AtlasProjectOpening.Ticket, cancellation: AtlasSearchCancellation, accessLease: AnyObject?) async {
        guard !Task.isCancelled, !cancellation.isCanceled, projectOpening.accepts(ticket),
              browserProject == ticket.project, projectBrowser.discoveryPolicy == discoveryPolicy,
              root.utf8.elementsEqual(requestedRoot.utf8), atlasDirectory == directory else { return }
        defer {
            if projectOpening.accepts(ticket) {
                projectOpening.fail(ticket)
                loadTask = nil; loadCancellation = nil; loadProgress = ""
            }
        }
        if projectOpening.catalog?.matchesDiscoveryPolicy(discoveryPolicy.wireName) != true
            || projectOpening.catalog?.requestedDirectory != directory {
            do {
                try await loadPreviewCatalog(root: requestedRoot, directory: directory,
                    discoveryPolicy: discoveryPolicy, ticket: ticket,
                    cancellation: cancellation, accessLease: accessLease)
            } catch {
                if !cancellation.isCanceled, projectOpening.accepts(ticket) {
                    status = "Atlas membership could not be prepared. Project Files remains available."
                }
                return
            }
        }
        guard !cancellation.isCanceled, projectOpening.accepts(ticket),
              browserProject == ticket.project, projectBrowser.discoveryPolicy == discoveryPolicy,
              atlasDirectory == directory else { return }
        // This cache owner supplies only Rust's highlighted-source cache. Its
        // legacy hot document table never owns the viewport's evicted sources.
        if projectCache?.root.utf8.elementsEqual(requestedRoot.utf8) != true {
            projectCache = AtlasProjectCache.defaultDirectory(root: requestedRoot).map {
                AtlasProjectCache(root: requestedRoot, cacheDirectory: $0)
            }
        }
        let request = UUID()
        atlasLayoutRequest = request
        atlasLayoutPending = true
        loadProgress = "Laying out file metadata…"
        let installed = await installAtlasScope(root: requestedRoot, project: ticket.project,
            request: request, cancellation: cancellation, accessLease: accessLease, preservingCaptures: false)
        guard projectOpening.accepts(ticket), atlasLayoutRequest == request else { return }
        atlasLayoutPending = false
        guard installed else { return }
        projectOpening.finishPreviews(ticket, available: true)
        loadTask = nil; loadCancellation = nil; loadProgress = ""
        // Source is admitted only after a visible native frame requests it.
        // Publishing metadata never dismisses the user's independent reader.
        if !searchPending {
            status = "Atlas ready for \(atlasEntries.count) known files. Source previews load as you navigate."
        }
    }

    private func cancelAtlasDirectory() {
        guard atlasDirectoryTarget != nil else { return }
        atlasLayoutRequest = UUID()
        atlasLayoutCancellation?.cancel(); atlasLayoutTask?.cancel(); atlasLayoutIO.cancel()
        atlasLayoutTask = nil; atlasLayoutCancellation = nil
        atlasLayoutPending = false; atlasDirectoryTarget = nil
        status = "Folder focus canceled. The current atlas remains available."
    }

    /// Discover and lay out a bounded directory while the accepted project,
    /// atlas and readers remain usable. Directory paths stay project-relative.
    private func requestAtlasDirectory(_ directory: AtlasDirectoryFocus, project: UUID,
        catalogRevision: UUID? = nil) {
        guard project == loadGeneration, canChangeAtlasDirectory,
              catalogRevision.map({ $0 == projectOpening.catalogRevision }) != false else { return }
        if directory == atlasDirectory, hasMetadataAtlas, !hasRetainedAtlas {
            showsCity = false
            status = "Atlas already focused on \(directory.display)."
                + (showsCatalog ? " Choose Show text atlas to view this folder." : "")
            return
        }
        guard let destination = try? AtlasFolderDestination(directory: directory, fileScope: fileScope,
            customExtensions: customExtensions) else {
            status = "Choose valid file types before changing the atlas folder."
            return
        }
        let move = atlasFolderNavigation.visit(destination, from: currentAtlasDestination())
        requestAtlasDestination(destination, project: project, move: move)
    }

    /// History and landmarks use the same transaction as explicit folder focus.
    /// The proposed origin is recorded only when the destination is published.
    private func requestAtlasDestination(_ destination: AtlasFolderDestination, project: UUID,
        move: AtlasFolderNavigation.Move) {
        guard project == loadGeneration, canChangeAtlasDirectory,
              atlasFolderNavigation.accepts(move) else { return }
        let directory = destination.directory
        if directory == atlasDirectory, hasMetadataAtlas, !hasRetainedAtlas,
           destination.matchesScope(fileScope: fileScope, customExtensions: customExtensions),
           atlasFolderNavigation.current?.directory == directory,
           atlasFolderNavigation.current?.matchesScope(fileScope: destination.fileScope,
                customExtensions: destination.customExtensions) == true {
            let resolved = resolvedAtlasDestination(destination, entries: atlasEntries)
            var acceptedNavigation = atlasFolderNavigation
            guard acceptedNavigation.commit(move, resolved: resolved), restoreAtlasPresentation(resolved) else { return }
            atlasFolderNavigation = acceptedNavigation
            addedFilesFocusRevision = nil
            atlasPreservesNavigation = true
            showsCity = false
            status = "Returned to \(directory.display)."
                + (destination.anchorRawPath != nil && resolved.anchorRawPath == nil
                    ? " The saved file is not in this catalog; showing the folder overview." : "")
                + (showsCatalog ? " Choose Show text atlas to view this location." : "")
            return
        }
        let browser = projectBrowser, requestedRoot = root, previousDirectory = atlasDirectory
        let previousCatalogRevision = projectOpening.catalogRevision
        let currentCatalog = projectOpening.catalog
        let originalScope = fileScope, originalExtensions = Array(customExtensions.utf8)
        let discoveryPolicy = browser.discoveryPolicy
        let accessLease: AnyObject?
#if FCB_APP_STORE
        guard !rootAccessLoader.isLoading, let access = rootAccess,
              access.url.path.utf8.elementsEqual(requestedRoot.utf8),
              browser.rootAccessIdentity == ObjectIdentifier(access) else {
            status = "Choose the project folder again to grant read access."
            return
        }
        accessLease = access
#else
        accessLease = nil
#endif
        let request = UUID(), cancellation = AtlasSearchCancellation()
        // Project Files may be mounted by the no-atlas fallback. Keep that
        // reader-owning view mounted when the first focused atlas is published.
        if !hasMetadataAtlas { showsCatalog = true }
        atlasLayoutRequest = request; atlasLayoutCancellation = cancellation
        atlasLayoutPending = true; atlasDirectoryTarget = directory
        atlasLayoutTask = Task {
            defer {
                if atlasLayoutRequest == request {
                    atlasLayoutTask = nil; atlasLayoutCancellation = nil
                    atlasLayoutPending = false; atlasDirectoryTarget = nil
                }
            }
            do {
                let catalog: AtlasProjectCatalog
                if directory == previousDirectory, let currentCatalog {
                    catalog = currentCatalog
                } else {
                    catalog = try await atlasLayoutIO.perform(cancellation: cancellation,
                        keepingAlive: accessLease.map { [$0] } ?? []) {
                        try AtlasProjectWorker.catalogReport(root: requestedRoot, directory: directory,
                            discoveryPolicy: discoveryPolicy, cancellation: cancellation)
                    }
                }
                guard !Task.isCancelled, !cancellation.isCanceled, atlasLayoutRequest == request,
                      loadGeneration == project, projectBrowser === browser, !browser.closed,
                      root.utf8.elementsEqual(requestedRoot.utf8), atlasDirectory == previousDirectory,
                      projectOpening.catalogRevision == previousCatalogRevision,
                      browser.discoveryPolicy == discoveryPolicy,
                      fileScope == originalScope, Array(customExtensions.utf8) == originalExtensions else { return }
                let installed = await installAtlasScope(root: requestedRoot, project: project, request: request,
                    cancellation: cancellation, accessLease: accessLease,
                    preservingCaptures: directory == previousDirectory,
                    replacementCatalog: catalog, folderMove: move)
                guard installed, atlasLayoutRequest == request else { return }
                // Keep Project Files mounted: a reader may have opened while
                // focus was preparing. Show text atlas remains an explicit action.
                showsCity = false
                status = "Atlas focused on \(directory.display). \(catalog.summary)"
                    + (destination.anchorRawPath != nil && atlasFolderNavigation.current?.anchorRawPath == nil
                        ? " The saved file is not in this catalog; showing the folder overview." : "")
                    + (showsCatalog ? " Choose Show text atlas to view this folder." : "")
            } catch {
                guard !Task.isCancelled, !cancellation.isCanceled, atlasLayoutRequest == request else { return }
                status = "The folder atlas could not be prepared. The current map and source readers remain available."
            }
        }
    }

    /// An explicit scope transaction may compact metadata on the worker. Source
    /// arrival and eviction never call this method or alter parcel geometry.
    private func installAtlasScope(root requestedRoot: String, project: UUID, request: UUID,
        cancellation: AtlasSearchCancellation, accessLease: AnyObject?, preservingCaptures: Bool,
        replacementCatalog: AtlasProjectCatalog? = nil,
        folderMove: AtlasFolderNavigation.Move? = nil) async -> Bool {
        let browser = projectBrowser, requestedDiscoveryPolicy = browser.discoveryPolicy
        let previousDirectory = atlasDirectory, previousCatalogRevision = projectOpening.catalogRevision
        guard let catalog = replacementCatalog ?? projectOpening.catalog,
              catalog.requestedRoot?.utf8.elementsEqual(requestedRoot.utf8) == true,
              catalog.matchesDiscoveryPolicy(requestedDiscoveryPolicy.wireName),
              replacementCatalog != nil || catalog.requestedDirectory == previousDirectory else { return false }
        let originalScope = fileScope, originalExtensions = Array(customExtensions.utf8)
        let requestedScope = folderMove?.target.fileScope ?? fileScope
        let requestedExtensions = folderMove?.target.customExtensions ?? customExtensions
        guard let destination = folderMove?.target ?? (try? AtlasFolderDestination(
            directory: catalog.requestedDirectory, fileScope: requestedScope,
            customExtensions: requestedExtensions)), destination.directory == catalog.requestedDirectory else { return false }
        let scopeLabel = requestedScope == .custom ? requestedExtensions : requestedScope.rawValue
        let entries = catalog.entries.filter { entry in
            entry.sourcePath.map { requestedScope.includes($0, custom: requestedExtensions) } ?? (requestedScope == .all)
        }
        let revision = UUID()
        let prepared: (entries: [AtlasProjectCatalog.Entry], layout: AtlasViewportLayout)
        do {
            prepared = try await atlasLayoutIO.perform(cancellation: cancellation,
                keepingAlive: accessLease.map { [$0] } ?? []) {
                guard !cancellation.isCanceled else { throw AtlasProjectIOError.canceled }
                guard let placed = AtlasParcelLayout.compact(entries) else { throw AtlasProjectIOError.invalidResponse }
                guard !cancellation.isCanceled else { throw AtlasProjectIOError.canceled }
                guard let layout = AtlasViewportLayout.prepare(root: requestedRoot, entries: placed, revision: revision) else {
                    throw AtlasProjectIOError.invalidResponse
                }
                guard !cancellation.isCanceled else { throw AtlasProjectIOError.canceled }
                return (entries: placed, layout: layout)
            }
        } catch {
            if !cancellation.isCanceled, atlasLayoutRequest == request, loadGeneration == project {
                status = "The atlas layout is unavailable. Project Files and source readers remain available."
            }
            return false
        }
        guard !Task.isCancelled, !cancellation.isCanceled, projectViewActive, loadGeneration == project,
              projectBrowser === browser, !browser.closed, browserProject == project,
              atlasLayoutRequest == request, root.utf8.elementsEqual(requestedRoot.utf8),
              browser.discoveryPolicy == requestedDiscoveryPolicy, atlasDirectory == previousDirectory,
              projectOpening.catalogRevision == previousCatalogRevision,
              fileScope == originalScope, Array(customExtensions.utf8) == originalExtensions else { return false }
#if FCB_APP_STORE
        guard !rootAccessLoader.isLoading,
              rootAccess.map(ObjectIdentifier.init) == accessLease.map(ObjectIdentifier.init),
              browser.rootAccessIdentity == accessLease.map(ObjectIdentifier.init),
              rootAccess?.url.path.utf8.elementsEqual(requestedRoot.utf8) == true else { return false }
#endif
        // A focused catalog remains private until both the opening state and
        // replacement layout can be admitted. No await splits publication.
        var acceptedOpening = projectOpening
        if replacementCatalog != nil {
            guard acceptedOpening.replaceAtlasCatalog(catalog, in: project,
                replacing: previousCatalogRevision, hasOpenableFiles: catalog.openableCount > 0) else { return false }
        }
        let compacted = prepared.entries
        let resolvedDestination = resolvedAtlasDestination(destination, entries: compacted)
        var acceptedNavigation = atlasFolderNavigation
        if let folderMove {
            guard acceptedNavigation.commit(folderMove, resolved: resolvedDestination) else { return false }
        } else {
            acceptedNavigation.rebaseCurrent(resolvedDestination)
        }
        let hadCurrentSearch = searchIsCurrent
        guard atlasResidency.install(prepared.layout, accessLease: accessLease,
            sourceCacheHandle: projectCache?.sourceHandle ?? 0,
            cacheLease: projectCache, preservingCaptures: preservingCaptures) else {
            status = "Atlas metadata could not be admitted. Project Files remains available."
            return false
        }
        if replacementCatalog != nil { projectOpening = acceptedOpening }
        atlasFolderNavigation = acceptedNavigation
        invalidateVisibleSearchScope()
        retainedAtlasRevision = nil
        atlasPreservesNavigation = false
        atlasRefreshSummary = ""; atlasAddedBounds = nil; atlasUnconfirmedPaths = []
        cityRefreshCamera = nil; cityPresentedCamera = nil
        atlasEntries = compacted
        atlasScopeLabel = scopeLabel
        let bounds = compacted.reduce(CGRect.null) { result, entry in
            result.union(CGRect(x: entry.x, y: entry.y, width: entry.width, height: entry.height))
        }
        contentBounds = bounds.isNull || bounds.isEmpty ? CGRect(x: 0, y: 0, width: 4096, height: 4096) : bounds
        camera.contentBounds = contentBounds
        camera.bindNavigation(to: revision)
        atlasRevision = revision
        if folderMove != nil {
            atlasPreservesNavigation = restoreAtlasPresentation(resolvedDestination)
            // Both onChange callbacks are qualified by the accepted revision
            // and exact filter value. A later user edit is never suppressed.
            atlasRestoredScope = (revision, resolvedDestination)
            let filterChanged = !resolvedDestination.matchesScope(fileScope: fileScope, customExtensions: customExtensions)
            fileScope = requestedScope; customExtensions = requestedExtensions
            if filterChanged { clearSearch() }
        }
        updateAtlasFiles(catalog)
        updateCatalogFilter()
        refreshResidentSearchOverlay(rebindingCurrentReport: hadCurrentSearch)
        if folderMove != nil, !query.isEmpty { runSearch(debounce: true) }
        return true
    }

    private func requestAtlasScopeLayout() {
        guard let destination = try? AtlasFolderDestination(directory: atlasDirectory,
            fileScope: fileScope, customExtensions: customExtensions) else {
            status = "Choose valid file types before rebuilding the atlas."
            return
        }
        let move = atlasFolderNavigation.visit(destination, from: currentAtlasDestination())
        atlasDirectoryTarget = nil
        invalidateVisibleSearchScope()
        atlasLayoutTask?.cancel(); atlasLayoutCancellation?.cancel(); atlasLayoutIO.cancel()
        atlasResidency.suspend()
        let request = UUID(), project = loadGeneration, requestedRoot = root
        let cancellation = AtlasSearchCancellation()
        atlasLayoutRequest = request; atlasLayoutCancellation = cancellation; atlasLayoutPending = true
#if FCB_APP_STORE
        let accessLease: AnyObject? = rootAccess
#else
        let accessLease: AnyObject? = nil
#endif
        atlasLayoutTask = Task {
            let installed = await installAtlasScope(root: requestedRoot, project: project, request: request,
                cancellation: cancellation, accessLease: accessLease, preservingCaptures: true, folderMove: move)
            guard atlasLayoutRequest == request else { return }
            atlasLayoutTask = nil; atlasLayoutCancellation = nil; atlasLayoutPending = false
            if installed, !searchPending {
                status = "\(atlasEntries.count) known files · \(atlasScopeLabel). Source loads for the visible area."
            }
        }
    }

    private func updateAtlasViewport(_ world: CGRect, scale: Double) {
        guard !showsCatalog, !showsCity, !atlasLayoutPending, hasMetadataAtlas, !hasRetainedAtlas else { return }
        if let viewport = try? AtlasVisibleSearchScope.Viewport(x: Double(world.minX), y: Double(world.minY),
            width: Double(world.width), height: Double(world.height)),
           visibleSearchView?.represents(root: root, project: loadGeneration,
                layout: atlasRevision, viewport: viewport, directory: atlasDirectory) != true {
            visibleSearchView = try? AtlasVisibleSearchScope.View(root: root,
                project: loadGeneration, layout: atlasRevision, viewport: viewport, directory: atlasDirectory)
        }
        atlasResidency.updateViewport(world, scale: scale, selectedPath: selectedPath)
    }

    private func cancelVisibleSearchFreeze() {
        visibleSearchRequest = UUID()
        visibleSearchCancellation?.cancel(); visibleSearchCancellation = nil
        visibleSearchTask?.cancel(); visibleSearchTask = nil
        visibleSearchIO.cancel()
    }

    private func cancelChoosingVisibleSearch() {
        cancelVisibleSearchFreeze()
        visibleSearchNotice = "Choosing visible files stopped. Use the current view to try again."
        searchTitle = "Visible scope canceled"; searchSummary = visibleSearchNotice; status = visibleSearchNotice
    }

    private func invalidateVisibleSearchScope() {
        cancelVisibleSearchFreeze()
        visibleSearchView = nil; frozenVisibleSearch = nil
        if searchPathKind == .visible {
            clearSearch()
            visibleSearchNotice = hasRetainedAtlas
                ? "Rebuild the retained atlas before choosing a refreshed visible-file search scope."
                : "The atlas layout changed. Choose a new displayed view before searching its files."
        } else { visibleSearchNotice = "" }
    }

    private func freezeVisibleSearch() {
        guard searchPathKind == .visible else { return }
        cancelVisibleSearchFreeze()
        frozenVisibleSearch = nil
        clearSearch()
        guard canFreezeVisibleSearch, let acceptedView = visibleSearchView else {
            visibleSearchNotice = AtlasVisibleSearchScope.Failure.invalidView.message
            searchSummary = visibleSearchNotice; searchTitle = "Choose an atlas view"
            return
        }
        let request = UUID(), cancellation = AtlasSearchCancellation(), entries = atlasEntries
        visibleSearchRequest = request; visibleSearchCancellation = cancellation
        visibleSearchNotice = "Choosing files from the displayed atlas view…"
        visibleSearchTask = Task {
            defer {
                if visibleSearchRequest == request {
                    visibleSearchTask = nil; visibleSearchCancellation = nil
                }
            }
            do {
                let frozen = try await visibleSearchIO.perform(cancellation: cancellation) {
                    let snapshot = try AtlasVisibleSearchScope.capture(view: acceptedView, entries: entries,
                        maximumFiles: AtlasSearchPathScope.maximumVisibleFiles,
                        maximumWireBytes: AtlasSearchPathScope.maximumVisibleWireBytes,
                        maximumPathBytes: AtlasSearchPathScope.maximumVisiblePathBytes,
                        id: request, canceled: { cancellation.isCanceled })
                    let pathScope = try AtlasSearchPathScope(visiblePaths: snapshot.paths)
                    guard !cancellation.isCanceled else { throw AtlasVisibleSearchScope.Failure.canceled }
                    let catalogScope = try AtlasSearchCatalogScope(directory: snapshot.view.directory.rawPath)
                    return AtlasFrozenViewportSearch(snapshot: snapshot, pathScope: pathScope, catalogScope: catalogScope)
                }
                guard !Task.isCancelled, !cancellation.isCanceled, visibleSearchRequest == request,
                      searchPathKind == .visible, !atlasLayoutPending, hasMetadataAtlas,
                      acceptedView.belongs(to: root, project: loadGeneration,
                        layout: atlasRevision, directory: atlasDirectory) else { return }
                frozenVisibleSearch = frozen
                visibleSearchNotice = ""
                if query.isEmpty {
                    searchSummary = "Frozen \(frozen.snapshot.count) atlas files. Enter a query to search their source."
                    status = searchSummary
                } else { runSearch(debounce: false) }
            } catch {
                guard visibleSearchRequest == request, !cancellation.isCanceled, !Task.isCancelled else { return }
                visibleSearchNotice = (error as? AtlasVisibleSearchScope.Failure)?.message
                    ?? (error as? AtlasSearchError)?.message ?? "The visible search scope could not be prepared."
                searchTitle = "Visible scope unavailable"; searchSummary = visibleSearchNotice; status = visibleSearchNotice
            }
        }
    }

    private func refreshResidentSearchOverlay(rebindingCurrentReport: Bool = false) {
        // Even a same-query stream must revalidate every geometry proof after
        // a source replacement or eviction. Query identity and readers survive.
        let canRebind = rebindingCurrentReport || searchIsCurrent
        let reportID = searchPresentation?.id
        overlayContentRevision = nil
        resolvedMatches = [:]; searchRows = []; searchPaths = []; selectedMatch = nil
        guard let report = searchReport, (report.indexStatus != nil) == wantsCapturedIndex,
              report.mode == searchMode, report.caseSensitive == effectiveSearchCaseSensitive,
              report.countAllMatches == effectiveSearchCountAllMatches,
              !loadingProject, canRebind else { return }
        prepareSearchOverlay(report, context: searchContext, retainingReportID: reportID)
        selectedMatch = selectedHit.flatMap { row in
            searchPresentation?.captureCandidate(for: row, in: searchContext) == nil ? nil : resolvedMatches[row.hit]
        }
    }

    private func suspendAtlasPreparation(suspendingResidency: Bool = true) {
        atlasDirectoryTarget = nil
        invalidateVisibleSearchScope()
        atlasLayoutRequest = UUID()
        atlasLayoutTask?.cancel(); atlasLayoutTask = nil
        atlasLayoutCancellation?.cancel(); atlasLayoutCancellation = nil
        atlasLayoutIO.cancel(); atlasLayoutPending = false
        if suspendingResidency { atlasResidency.suspend() }
    }

    private func closeAtlasResidency() {
        suspendAtlasPreparation()
        retainedAtlasRevision = nil
        atlasPreservesNavigation = false
        atlasRefreshSummary = ""; atlasAddedBounds = nil; atlasUnconfirmedPaths = []
        cityRefreshCamera = nil; cityPresentedCamera = nil
        atlasResidency.close(); atlasEntries = []
    }

    private func applyFileScope() {
        guard !isRestoredAtlasScope else { return }
        atlasRestoredScope = nil
        if atlasDirectoryTarget != nil { cancelAtlasDirectory() }
        readerHistory.catalogRefreshed()
        cancelSourceLoad()
        clearSearch()
        if usesPagedReader, readerHistory.current != nil { presentWorkspaceReader() }
        else { selectedPath = nil; selectedSource = nil; selectedMatch = nil }
        updateCatalogFilter()
        if hasRetainedAtlas {
            if projectOpening.isPreparing { cancelAtlasLoad() }
            requestAtlasPreviews()
            status = "Rebuilding the earlier atlas for \(fileScope.rawValue) scope. Project Files and search use the selected file types."
        } else if hasMetadataAtlas {
            requestAtlasScopeLayout()
            status = "Preparing \(fileScope.rawValue) scope. Search uses the selected file types."
        } else if projectOpening.isPreparing, projectOpening.catalog != nil {
            // Coalesce a scope edit while the initial metadata layout runs.
            cancelAtlasLoad()
            requestAtlasPreviews()
        } else {
            status = "\(fileScope.rawValue) scope. Project Files and search use the selected file types."
        }
        if !loadingProject && !query.isEmpty { runSearch(debounce: true) }
    }

    private func clearSearch() {
        if sourceSearchRow != nil {
            cancelSourceLoad()
            if selectedSource == nil, workspaceReader == nil {
                selectedPath = nil
                fileText = ""
                sourceError = nil
            }
        }
        searchCoordinator?.cancel()
        searchPending = false; searchWindowPending = false
        hits = []; selectedHit = nil; selectedMatch = nil
        selectedReaderSelection = nil
        searchReport = nil; searchPresentation = nil
        resolvedMatches = [:]; searchRows = []; searchPaths = []
        overlayContentRevision = nil
        overlayLimited = false
        searchTitle = "Search project"
        searchSummary = "Enter text to search the project."
        status = "\(hasRetainedAtlas ? "Earlier atlas: " : "")\(displayedFiles.count) of \(files.count) files shown. Search cleared."
    }

    private func prepareSearchOverlay(_ report: AtlasSearchReport, context: AtlasSearchContext,
        retainingReportID: UUID? = nil) {
        // Only a validated append-only stream in the same immutable atlas may
        // reuse geometry. New queries/scopes/captures start from no proof.
        let sameStream = report.streamID != nil && report.streamID == searchPresentation?.id
        var matches: [SearchHit.ID: AtlasMatch] = !hasRetainedAtlas && sameStream && searchIsCurrent ? resolvedMatches : [:]
        let grouped = Dictionary(grouping: report.hits.compactMap { hit in
            hit.sourcePath.map { ($0, hit) }
        }, by: { Array($0.0.utf8) })
        for pathBytes in grouped.keys.sorted(by: { $0.lexicographicallyPrecedes($1) }) {
            guard !hasRetainedAtlas, let path = String(bytes: pathBytes, encoding: .utf8),
                  fileScope.includes(path, custom: customExtensions),
                  let document = atlasDocuments[path],
                  document.source.path.utf8.elementsEqual(pathBytes),
                  let entries = grouped[pathBytes] else { continue }
            // A single workspace query names one capture per file. Reject any
            // inconsistent identity rather than borrowing another hit's proof.
            guard let first = entries.first?.1, let digest = first.captureSHA256,
                  let count = first.captureByteLength else { continue }
            let valid = entries.map(\.1).filter {
                matches[$0.id] == nil && $0.captureSHA256 == digest && $0.captureByteLength == count
            }
            guard !valid.isEmpty else { continue }
            let resolved = ProcessInfo.processInfo.environment["FCB_BATCH_MATCHES"] == "0"
                ? valid.map { AtlasMatch.resolve(document: document, byteStart: $0.start, byteEnd: $0.end,
                    expectedSHA256: digest, expectedByteCount: count) }
                : AtlasMatch.resolveBatch(document: document, ranges: valid.map { ($0.start, $0.end) },
                    expectedSHA256: digest, expectedByteCount: count)
            for (hit, match) in zip(valid, resolved) { if let match { matches[hit.id] = match } }
        }
        var rows: [CGRect] = []
        var seen: Set<String> = []
        var limited = matches.count != report.hits.count
        for hit in report.hits {
            for rect in matches[hit.id]?.rowRects ?? [] {
                let key = "\(rect.minX),\(rect.minY),\(rect.width),\(rect.height)"
                if seen.contains(key) { continue }
                if rows.count < 256 { seen.insert(key); rows.append(rect) } else { limited = true }
            }
        }
        resolvedMatches = matches; searchRows = rows
        overlayContentRevision = atlasResidency.contentRevision
        searchPresentation = AtlasSearchPresentation(context: context, hits: report.hits,
            verifiedHitIDs: Set(matches.keys), id: report.streamID ?? retainingReportID ?? UUID())
        searchPaths = Set(report.hits.compactMap(\.sourcePath))
        overlayLimited = limited
    }

    /// A reader supplies only a verified spelling. This closure captures the
    /// displayed project's authority; a queued action cannot borrow a new root.
    private var selectedSourceProjectSearch: ((AtlasReaderSelectionQuery) -> Void)? {
        guard !loadingProject, !root.isEmpty else { return nil }
        let requestedRoot = root, requestedProject = browserProject
        let browser = projectBrowser
        return { selected in
            guard !loadingProject, browserProject == requestedProject, projectBrowser === browser,
                  root.utf8.elementsEqual(requestedRoot.utf8) else { return }
            searchFromSelectedSource(text: selected.text, mode: selected.mode)
        }
    }

    /// A bounded observation supplies exact selected text, not an immutable
    /// source capture. Only this sheet's current project grant permits using
    /// that spelling as a new project query.
    private func observedSourceProjectSearch(for presentation: AtlasObservedReaderPresentation)
        -> ((AtlasObservedReaderSelectionQuery) -> Void)? {
        let source = presentation.source
        guard source.input.route == .project, let authority = snapshotExportSource,
              source.identity.project == authority.identity.project,
              source.input.root.utf8.elementsEqual(authority.root.utf8),
              source.identity.grant == authority.access.map(ObjectIdentifier.init) else { return nil }
        let browser = projectBrowser
        return { selected in
            guard observedReader?.id == presentation.id, projectBrowser === browser,
                  selected.authority == source.identity,
                  presentation.model.textSelection?.id == selected.selectionID,
                  presentation.model.page?.observation == selected.observation,
                  let current = snapshotExportSource,
                  current.identity == authority.identity,
                  current.root.utf8.elementsEqual(authority.root.utf8),
                  current.access.map(ObjectIdentifier.init) == source.identity.grant else { return }
            searchFromSelectedSource(text: selected.text, mode: selected.mode)
        }
    }

    private func searchFromSelectedSource(text: String, mode: AtlasSearchMode) {
        // Detach the current retained reader from the old result row before
        // replacing that query. Its capture and navigation remain mounted.
        cancelSourceLoad()
        clearSearch()
        let changesInput = searchMode != mode || !query.utf8.elementsEqual(text.utf8)
            || (mode.supportsNameMatching && searchNameMode != .exact)
            || (mode.isTextual && !searchCaseSensitive)
        searchMode = mode
        if mode.supportsNameMatching { searchNameMode = .exact }
        if mode.isTextual { searchCaseSensitive = true }
        query = text
        showsSidebar = true
        searchFocused = false
        // Changed controls enter the existing bounded/debounced generation
        // path. Repeating an unchanged action still starts a fresh query.
        if !changesInput { runSearch() }
    }

    private func runSearch() { runSearch(debounce: false) }

    private func runSearch(debounce: Bool, window: AtlasSearchResultWindowRequest? = nil) {
        if let window {
            guard wantsCapturedIndex, searchIsCurrent, !searchPending,
                  let accepted = searchReport?.resultWindow,
                  accepted.source.index == window.source.index,
                  accepted.previous?.offset == window.offset || accepted.next?.offset == window.offset else { return }
        }
        let requestedText = query
        let requestedRoot = root
        guard !loadingProject, !requestedText.isEmpty, !requestedRoot.isEmpty else { return }
        let visibleScope = currentVisibleSearch
        if searchPathKind == .visible, visibleScope == nil {
            clearSearch()
            searchTitle = visibleSearchTask == nil ? "Choose an atlas view" : "Choosing visible files"
            searchSummary = visibleSearchTask == nil ? "Choose the displayed atlas view before searching its files."
                : "Choosing files from the displayed view; the query will start when its scope is ready."
            status = searchSummary
            return
        }
        let requestedContext = searchContext
#if FCB_APP_STORE
        guard let access = rootAccess, access.url.path.utf8.elementsEqual(requestedRoot.utf8) else {
            clearSearch()
            status = "Choose the project folder again to grant read access."
            searchSummary = status
            searchTitle = "Project access unavailable"
            return
        }
        let accessLease: AnyObject? = access
#else
        let accessLease: AnyObject? = nil
#endif
        do {
            let pathScope: AtlasSearchPathScope
            if searchPathKind == .visible {
                guard let visibleScope else { throw AtlasSearchError.invalidPathScope }
                pathScope = visibleScope.pathScope
            } else {
                pathScope = try AtlasSearchPathScope(kind: searchPathKind,
                    path: searchPathKind == .workspace ? "" : searchPathInput)
            }
            let input = try AtlasSearchInput(root: requestedRoot, query: requestedText,
                fileScope: fileScope.searchScope(custom: customExtensions),
                pathScope: pathScope,
                mode: searchMode, nameMode: searchMode.effectiveNameMode(searchNameMode),
                caseSensitive: effectiveSearchCaseSensitive, countAllMatches: effectiveSearchCountAllMatches,
                discoveryPolicy: requestedContext.discoveryPolicy,
                catalogScope: AtlasSearchCatalogScope(kind: requestedContext.catalogKind,
                    rawPath: requestedContext.catalogPath), resultWindow: window)
            if window == nil { clearSearch() }
            searchPending = true; searchWindowPending = window != nil
            searchTitle = input.pathScope.isVisible ? "Searching frozen atlas files" : "Searching project"
            searchSummary = input.pathScope.isVisible
                ? "Searching \(visibleScope?.snapshot.count ?? 0) files from the frozen atlas view. Moving the camera does not change this query's membership."
                : input.isWorkspaceScope
                ? "Searching captured source text. You can navigate the atlas or cancel."
                : (input.pathScope.isWorkspace
                    ? "Searching selected file types. Excluded files do not consume the query's scan or result limits."
                    : "Searching \(input.pathScope.kind.label.lowercased()) \(String(reflecting: input.pathScope.path))\(input.fileScope.isAll ? "." : " within selected file types.") Excluded files do not consume the query's scan or result limits.")
            if input.mode == .expression {
                searchSummary += " " + input.mode.explanation
            } else if !input.mode.isTextual {
                searchSummary = "Searching \(input.mode.resultLabel). \(input.mode.explanation)"
                if input.pathScope.isVisible {
                    searchSummary += " Scope: \(visibleScope?.snapshot.count ?? 0) files from the frozen atlas view."
                }
            }
            if let window {
                searchTitle = "Finding result window"
                searchSummary = "Verifying matches from \(window.offset + 1) in the same captured files. The accepted results and reader stay available."
            }
            status = searchSummary
            let coordinator = searchCoordinator ?? AtlasSearchCoordinator(scopedWork: AtlasNativeSearch.runScoped)
            searchCoordinator = coordinator
            let service = workspaceSearch ?? AtlasWorkspaceSearch(scopedLive: AtlasNativeSearch.runScoped,
                makeIndex: { AtlasIndexedSearch(transport: .native) })
            workspaceSearch = service
            let choice = service.selection(indexed: wantsCapturedIndex)
            try coordinator.submitScoped(input: input, accessLease: accessLease, debounce: debounce, using: choice.scopedWork) { result in
                // Generation rejection happens in the coordinator. These checks
                // also cover SwiftUI state changes before onChange has run.
                guard !loadingProject, requestedContext.matchesSourceRequest(searchContext),
                      workspaceSearch === service, service.accepts(choice),
                      choice.indexed == wantsCapturedIndex else { return }
                searchPending = false
                switch result {
                case .success(let report):
                    if let window {
                        guard let received = report.resultWindow,
                              received.offset == window.offset,
                              received.source.index == window.source.index else {
                            searchWindowPending = false
                            status = AtlasSearchError.invalidResponse.message
                            searchSummary = status + " The previous results remain available."
                            return
                        }
                        if report.isInProgress {
                            searchPending = true
                            searchSummary = report.summary + " The previous result window remains displayed."
                            status = searchSummary
                            return
                        }
                    }
                    searchWindowPending = false
                    guard report.discoveryPolicy == requestedContext.discoveryPolicy,
                          report.catalogScope == input.catalogScope else {
                        status = AtlasSearchError.invalidResponse.message
                        searchSummary = status; searchTitle = "Search unavailable"
                        return
                    }
                    if window != nil { selectedHit = nil; selectedMatch = nil }
                    searchPending = report.isInProgress
                    prepareSearchOverlay(report, context: searchContext)
                    searchReport = report
                    hits = report.hits
                    status = report.summary
                    searchSummary = report.summary
                    searchTitle = report.indexStatus?.building == true ? "Preparing captured index"
                        : (report.isInProgress
                            ? (report.pathScope.isVisible ? "Searching frozen atlas files" : "Searching project")
                            : (report.complete ? "No matches" : "Partial search"))
                case .failure(let error):
                    searchWindowPending = false
                    status = error.message
                    searchSummary = status + (window == nil ? "" : " The previous result window and reader are unchanged.")
                    if case .canceled = error { searchTitle = "Search canceled" }
                    else { searchTitle = "Search unavailable" }
                }
            }
        } catch {
            searchPending = false; searchWindowPending = false
            status = (error as? AtlasSearchError)?.message ?? "Search failed. Results are unavailable."
            searchSummary = status
            searchTitle = "Search unavailable"
        }
    }

    private func refreshCapturedSearch() {
        guard wantsCapturedIndex, !loadingProject, !root.isEmpty else { return }
        clearSearch()
        workspaceSearch?.refresh()
        if query.isEmpty {
            searchSummary = "Captured index cleared. Enter a query to capture the current file and location scope; an opened reader keeps its original bytes."
            status = searchSummary
        } else { runSearch() }
    }

    private func cancelSearch() {
        searchCoordinator?.cancel()
        let preservedWindow = searchWindowPending
        searchPending = false; searchWindowPending = false
        searchTitle = "Search canceled"
        searchSummary = AtlasSearchError.canceled.message
            + (preservedWindow ? " The previous result window and reader are unchanged." : "")
        status = searchSummary
    }

    private func moveSearchResult(backwards: Bool) {
        guard searchIsCurrent, let presentation = searchPresentation,
              let row = presentation.adjacentRow(in: hits.map(\.id), after: selectedHit,
                  backwards: backwards, context: searchContext),
              let path = presentation.captureCandidate(for: row, in: searchContext)?.sourcePath else {
            status = "No navigable matches in the current search and file filter."
            return
        }
        showsReader = true
        if selectedHit == row {
            // A single remaining match still responds to explicit navigation.
            openFile(path, searchRow: row, exactMatch: true)
        } else {
            // The same guarded activation path as a clicked result row.
            selectedHit = row
        }
    }

    /// Give the paged reader the displayed immutable bytes when available.
    /// It owns fresh reader state and never inherits atlas/search coordinates.
    /// Only an explicit later Refresh from disk can replace this source.
    private func openPagedFile(_ path: String) {
        guard !loadingProject, selectedPath?.utf8.elementsEqual(path.utf8) == true,
              acceptsSource(path, searchRow: sourceSearchRow, exactMatch: false) else { return }
        let supplied: AtlasSuppliedReaderSource?
        do {
            if let source = selectedSource {
                guard source.path.utf8.elementsEqual(path.utf8) else {
                    status = "The displayed capture belongs to a different file. Open the intended source again."
                    return
                }
                supplied = try AtlasSuppliedReaderSource(root: root, path: path, text: source.text)
            } else { supplied = nil }
        } catch {
            status = (error as? AtlasReaderError)?.message ?? "The displayed source could not be opened as retained pages."
            return
        }
        openWorkspaceReader(path, supplied: supplied)
    }

    private func openSearchFile(_ row: AtlasSearchRowID) {
        guard searchIsCurrent,
              let path = searchPresentation?.filePath(for: row, in: searchContext) else {
            status = AtlasSearchHitAvailability.stale.message
            return
        }
        selectedHit = nil
        openFile(path, searchRow: row)
        showsReader = true
    }

    private func cancelSourceLoad() {
        sourceTask?.cancel()
        sourceCancellation?.cancel()
        sourceIO.cancel()
        sourceTask = nil
        sourceCancellation = nil
        sourceSearchRow = nil
    }

    private func acceptsSource(_ path: String, searchRow: AtlasSearchRowID?, exactMatch: Bool) -> Bool {
        guard let row = searchRow else { return true }
        guard searchIsCurrent,
              let presentation = searchPresentation,
              let sourcePath = presentation.filePath(for: row, in: searchContext),
              sourcePath.utf8.elementsEqual(path.utf8) else { return false }
        return !exactMatch || (selectedHit == row
            && presentation.captureCandidate(for: row, in: searchContext) != nil)
    }

    private func openFile(_ path: String, searchRow: AtlasSearchRowID? = nil,
                          exactMatch: Bool = false, initialLine: UInt64? = nil) {
        guard !loadingProject, acceptsSource(path, searchRow: searchRow, exactMatch: exactMatch) else { return }
        if exactMatch, let capture = searchReport?.capture {
            guard let row = searchRow,
                  let hit = searchPresentation?.captureCandidate(for: row, in: searchContext),
                  let target = capture.target(hit), target.root.utf8.elementsEqual(root.utf8),
                  target.path.utf8.elementsEqual(path.utf8),
                  target.needle.utf8.elementsEqual((hit.actualText ?? searchReport?.expression?.primaryNeedle ?? query).utf8),
                  searchMode != .expression || searchReport?.expression?.query.utf8.elementsEqual(query.utf8) == true,
                  target.mode == searchMode, target.caseSensitive == effectiveSearchCaseSensitive else {
                status = "The retained search capture could not authorize this match. Your current reader is unchanged. Search again, or explicitly open the current file."
                return
            }
            // This current row authorizes one independent capture transfer.
            // Query edits cannot revoke its accepted reader or lend it a path.
            openWorkspaceReader(path, captured: target)
            return
        }
        if !exactMatch {
            let supplied: AtlasSuppliedReaderSource?
            do {
                // String dictionaries use canonical equality; original path
                // bytes must match before an atlas lends us its exact capture.
                if !hasRetainedAtlas, let source = atlasDocuments[path]?.source,
                   source.path.utf8.elementsEqual(path.utf8) {
                    supplied = try AtlasSuppliedReaderSource(root: root, path: path, text: source.text)
                } else { supplied = nil }
            } catch {
                status = "The displayed source could not be opened as retained pages. Your current reader is unchanged."
                return
            }
            openWorkspaceReader(path, supplied: supplied, initialLine: initialLine)
            return
        }

        // Legacy reports without a transferable capture keep their existing
        // exact digest/range verification. Retained history remains available
        // through Source Back, and is never replaced by an unverified result.
        readerHistory.cancel()
        cancelSourceLoad()
        sourceSearchRow = searchRow
        if !hasRetainedAtlas, let source = atlasDocuments[path]?.source,
           source.path.utf8.elementsEqual(path.utf8) {
            presentSource(source, searchRow: searchRow, exactMatch: true)
            sourceSearchRow = nil
            return
        }

        // Preserve the bounded native queue and root lease for on-demand reads.
        // Revalidate the report as well as the project before any publication.
        let requestedRoot = root
        let generation = loadGeneration
        let cancellation = AtlasSearchCancellation()
        sourceCancellation = cancellation
        status = "Opening \(path)…"
        let leases: [AnyObject]
#if FCB_APP_STORE
        guard let access = rootAccess, access.url.path.utf8.elementsEqual(requestedRoot.utf8) else {
            sourceCancellation = nil
            sourceUnavailable()
            return
        }
        leases = [access]
#else
        leases = []
#endif
        sourceTask = Task {
            defer {
                if sourceCancellation === cancellation {
                    sourceTask = nil
                    sourceCancellation = nil
                    sourceSearchRow = nil
                }
            }
            do {
                let text = try await sourceIO.perform(cancellation: cancellation, keepingAlive: leases) {
                    Engine.read(root: requestedRoot, path: path)
                }
                guard !Task.isCancelled, !cancellation.isCanceled,
                      sourceCancellation === cancellation, loadGeneration == generation,
                      root.utf8.elementsEqual(requestedRoot.utf8),
                      acceptsSource(path, searchRow: searchRow, exactMatch: exactMatch) else { return }
                if let text {
                    presentSource(AtlasSource(path: path, text: text), searchRow: searchRow, exactMatch: exactMatch)
                } else { sourceUnavailable() }
            } catch {
                guard !Task.isCancelled, !cancellation.isCanceled,
                      sourceCancellation === cancellation, loadGeneration == generation,
                      root.utf8.elementsEqual(requestedRoot.utf8),
                      acceptsSource(path, searchRow: searchRow, exactMatch: exactMatch) else { return }
                sourceUnavailable()
            }
        }
    }

    private func presentSource(_ source: AtlasSource, searchRow: AtlasSearchRowID?, exactMatch: Bool) {
        guard acceptsSource(source.path, searchRow: searchRow, exactMatch: exactMatch) else { return }
        let hit: SearchHit?
        let range: NSRange?
        if exactMatch {
            guard let row = searchRow,
                  let candidate = searchPresentation?.captureCandidate(for: row, in: searchContext),
                  let digest = candidate.captureSHA256, let count = candidate.captureByteLength,
                  let verified = AtlasMatch.resolveSourceRange(source: source, byteStart: candidate.start,
                      byteEnd: candidate.end, expectedSHA256: digest, expectedByteCount: count) else {
                // An exact-hit request must not silently become a file-start
                // navigation. The separate Open file action remains available.
                status = "Exact match unavailable: the source capture changed or its offsets cannot be mapped. Your current reader is unchanged. Search again, or use Open file without match navigation."
                return
            }
            hit = candidate
            range = verified
        } else {
            hit = nil
            range = nil
        }
        usesPagedReader = false
        selectedPath = source.path
        selectedMatch = nil
        focusRequest = UUID()
        showsReader = true
        fileText = source.text
        selectedSource = source
        sourceError = nil
        selectedReaderSelection = range.map { AtlasReaderSelection(source: source, range: $0) }
        guard let hit, let range else {
            status = searchRow == nil
                ? "Opened \(source.path). Zoom into its parcel to read the captured source."
                : "Opened \(source.path) without match navigation. No exact search location was used."
            return
        }
        // Reader verification is independent of the overlay's row budget and
        // eager capture membership. Only verified geometry adds atlas bands.
        if let match = resolvedMatches[hit.id], match.sourceRange == range {
            selectedMatch = match
            searchRows = Array((match.rowRects + searchRows.filter { !match.rowRects.contains($0) }).prefix(256))
            status = "Matching source rows highlighted in the captured file."
        } else {
            status = "Exact match located in captured source; atlas highlighting is unavailable."
        }
    }

    private func sourceUnavailable() {
        status = "Source unavailable: this legacy match reader accepts UTF-8 files up to 4 MiB, without embedded NUL. The file may have changed or become inaccessible. Your current reader is unchanged."
    }

#if FCB_APP_STORE
    private func activateProject(_ access: AppStoreRootAccess) {
        nativeOpen.closeReader()
        // Replacing the native grant revokes this window's pin deliveries even
        // when the newly authorized folder happens to have the same path.
        projectChanges.close()
        pinnedReaders.closeAll()
        access.remember()
        rootAccess = access
        root = access.url.path
        clearSearch()
        searchTitle = "Search project"
        searchSummary = "Enter text to search the project."
        selectedPath = nil
        replaceProject()
    }

    private func restoreProject(_ path: String) {
        nativeOpen.reserve()
        guard let request = AppStoreRootAccess.restorationRequest(path) else {
            rootAccessLoader.cancel()
            status = "Saved project access is unavailable. Choose the folder again to restore it."
            return
        }
        requestRootAccess(request)
    }

    private func requestRootAccess(_ request: AtlasRootAccessRequest, ifCurrent ticket: UUID? = nil,
        keepingAlive leases: [AnyObject] = []) {
        if let ticket, !rootAccessLoader.accepts(ticket) { return }
        status = "Opening project access…"
        rootAccessLoader.open(request, ifCurrent: ticket, keepingAlive: leases) { result in
            switch result {
            case .success(let access): activateProject(access)
            case .failure:
                status = "Project access is unavailable. Reconnect the volume if needed, then choose the folder again."
            }
        }
    }
#else
    private func setRoot(_ newRoot: String) {
        nativeOpen.closeReader()
        projectChanges.close()
        root = newRoot
        UserDefaults.standard.set(newRoot, forKey: "fcb.root")
        Engine.remember(root: newRoot)
        clearSearch()
        searchTitle = "Search project"
        searchSummary = "Enter text to search the project."
        selectedPath = nil
        replaceProject()
    }
#endif

    private func chooseProject() { chooseNativeItem(allowFiles: false) }

    private func receiveNativeOpen(_ event: AtlasNativeOpenRouting.Event) {
        switch event {
        case .choose: chooseNativeItem(allowFiles: true)
        case .open(let url): openNativeItem([url], origin: .external)
        case .invalid: nativeOpen.reject(AtlasNativeOpenError.invalidSelection.message)
        }
    }

    private func openNativeItem(_ urls: [URL], origin: AtlasNativeOpenOrigin, ifCurrent ticket: UUID? = nil) {
        guard projectViewActive, ticket == nil || nativeOpen.accepts(ticket!) else { return }
#if FCB_APP_STORE
        rootAccessLoader.cancel()
#endif
        nativeOpen.open(urls, origin: origin, ifCurrent: ticket) { target in
            guard case .folder(let selected) = target else { return }
#if FCB_APP_STORE
            activateProject(selected)
#else
            setRoot(selected.path)
#endif
        }
    }

    private func chooseNativeItem(allowFiles: Bool) {
        guard projectViewActive else { return }
        let intent = nativeOpen.reserve()
#if FCB_APP_STORE
        rootAccessLoader.cancel()
#endif
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = allowFiles
        panel.allowsMultipleSelection = false
        panel.resolvesAliases = false
        panel.directoryURL = root.isEmpty ? FileManager.default.homeDirectoryForCurrentUser : URL(fileURLWithPath: root)
        panel.message = allowFiles
            ? "Open one file for reading, or choose a project folder to browse"
            : "Choose the project root to lay out and browse"
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            guard projectViewActive, nativeOpen.accepts(intent) else {
#if FCB_APP_STORE
                // Drain the panel's automatically started grant even when its
                // callback belongs to an older choice or a closed window.
                _ = AppStoreRootAccess.selectionRequest(url)
#endif
                return
            }
            guard AtlasNativeOpenRequest.selectedURL([url]) != nil else {
#if FCB_APP_STORE
                _ = AppStoreRootAccess.selectionRequest(url)
#endif
                nativeOpen.reject(AtlasNativeOpenError.invalidSelection.message)
                return
            }
            openNativeItem([url], origin: .panel, ifCurrent: intent)
        }
    }

    private func acceptNativeDrop(_ providers: [NSItemProvider]) -> Bool {
        guard projectViewActive, providers.count == 1, let provider = providers.first,
              provider.canLoadObject(ofClass: NSURL.self) else {
            nativeOpen.reject(AtlasNativeOpenError.invalidSelection.message)
            return false
        }
        let intent = nativeOpen.reserve()
#if FCB_APP_STORE
        rootAccessLoader.cancel()
#endif
        nativeOpen.waitForDrop(intent)
        nativeDrop.submit(provider) { result in
            guard projectViewActive, nativeOpen.accepts(intent) else { return }
            guard case .success(let url) = result,
                  AtlasNativeOpenRequest.selectedURL([url]) != nil else {
                nativeOpen.rejectDrop(intent)
                return
            }
            openNativeItem([url], origin: .external, ifCurrent: intent)
        }
        return true
    }

}

// MARK: - Atlas canvas (display-link driven, text at every zoom)

struct AtlasCanvas: View {
    let active: Bool
    let preservesNavigation: Bool
    let revision: UUID
    let contentRevision: UUID
    let focusRequest: UUID
    let selectedMatch: AtlasMatch?
    let searchRows: [CGRect]
    let scopeLabel: String
    let files: [AtlasFile]
    let metadata: [AtlasProjectCatalog.Entry]
    let camera: AtlasCamera
    let hitPaths: Set<String>
    let selectedPath: String?
    let contentBounds: CGRect
    let onSelect: (AtlasProjectCatalog.Entry) -> Void
    let onViewport: (CGRect, Double) -> Void
    let residencySummary: String
    let retryPreviews: () -> Void
    let tiles: [AtlasTextTile]

    @GestureState private var dragDelta: CGSize = .zero
    @State private var viewport: CGSize = .zero
    @State private var initialFitPending = true
    @State private var presentedFrame = AtlasPresentedFrame()
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        GeometryReader { geometry in
            let size = geometry.size
            TimelineView(.animation(paused: !active || !camera.isAnimating)) { timeline in
                AtlasRetainedView(tiles: tiles, revision: revision, scale: camera.scale,
                    offset: CGPoint(x: camera.offsetX, y: camera.offsetY),
                    selectedPath: selectedPath, hitPaths: hitPaths, matchRows: searchRows.isEmpty ? (selectedMatch?.rowRects ?? []) : searchRows,
                    metadata: metadata, contentRevision: contentRevision,
                    onPresentedContent: { acceptedLayout, acceptedContent, scale, offset in
                        guard active, acceptedLayout == revision, acceptedContent == contentRevision,
                              camera.acceptsNavigation(revision: revision) else { return }
                        presentedFrame.record(revision: revision, scale: scale, offset: offset,
                            contentRevision: contentRevision)
                        requestVisibleSource(in: size)
                    })
                .onChange(of: timeline.date) { _, _ in
                    guard active, camera.acceptsNavigation(revision: revision) else { return }
                    camera.tick(now: ProcessInfo.processInfo.systemUptime, viewport: size)
                }
            }
            .contentShape(Rectangle())
            .clipped()
            .gesture(
                DragGesture(minimumDistance: 0)
                    .updating($dragDelta) { value, state, _ in
                        state = value.translation
                    }
                    .onChanged { value in
                        guard active, camera.acceptsNavigation(revision: revision) else { return }
                        camera.drag(location: value.location, start: value.startLocation,
                                    now: ProcessInfo.processInfo.systemUptime)
                    }
                    .onEnded { _ in
                        guard active, camera.acceptsNavigation(revision: revision) else { return }
                        camera.endPan(now: ProcessInfo.processInfo.systemUptime)
                    }
            )
            .simultaneousGesture(
                SpatialTapGesture(count: 2)
                    .onEnded { value in
                        guard active, camera.acceptsNavigation(revision: revision) else { return }
                        camera.steerZoom(by: 2.2, at: value.location)
                    }
            )
            .simultaneousGesture(
                SpatialTapGesture()
                    .onEnded { value in
                        guard active, camera.acceptsNavigation(revision: revision), dragDelta == .zero,
                              let point = presentedFrame.worldPoint(at: value.location, revision: revision,
                                  contentRevision: contentRevision),
                              let file = metadata.last(where: {
                                  CGRect(x: $0.x, y: $0.y, width: $0.width, height: $0.height).contains(point)
                              }) else {
                            return
                        }
                        onSelect(file)
                    }
            )
            .overlay {
                if active {
                AtlasInputOverlay(onZoom: { factor, point, immediate in
                    guard camera.acceptsNavigation(revision: revision) else { return }
                    camera.steerZoom(by: factor, at: point, immediate: immediate)
                }, onWheel: { delta, point, precise in
                    guard camera.acceptsNavigation(revision: revision) else { return }
                    camera.steerWheelZoom(delta: delta, precise: precise, at: point)
                })
                }
            }
            .onAppear {
                camera.reducedMotion = reduceMotion
            }
            .onChange(of: reduceMotion) { _, value in camera.reducedMotion = value }
            .onChange(of: revision, initial: true) { _, value in
                guard active, camera.acceptsNavigation(revision: value) else { return }
                initialFitPending = !preservesNavigation && !camera.fit(bounds: contentBounds, viewport: size)
            }
            .onChange(of: focusRequest) { _, _ in
                guard active, let path = selectedPath,
                      let file = metadata.first(where: { $0.rawPath.elementsEqual(path.utf8) }) else { return }
                let parcel = CGRect(x: file.x, y: file.y, width: file.width, height: file.height)
                camera.navigateFocus(bounds: selectedMatch?.focusRect ?? parcel, viewport: size, revision: revision)
            }
            .onChange(of: selectedPath.map { Array($0.utf8) }) { _, _ in requestVisibleSource(in: size) }
            .onChange(of: size, initial: true) { _, newSize in
                viewport = newSize
                guard active, camera.acceptsNavigation(revision: revision) else { return }
                camera.updateViewport(newSize)
                // onAppear can precede the first usable GeometryReader size.
                // Retry until fit succeeds, then leave navigation under user control.
                if initialFitPending && !preservesNavigation {
                    initialFitPending = !camera.fit(bounds: contentBounds, viewport: newSize)
                }
            }
            .onChange(of: active) { _, value in
                guard value, camera.acceptsNavigation(revision: revision) else { return }
                camera.updateViewport(size)
                if initialFitPending && !preservesNavigation { initialFitPending = !camera.fit(bounds: contentBounds, viewport: size) }
                requestVisibleSource(in: size)
            }
            .overlay(alignment: .bottomLeading) {
                HStack(spacing: 8) {
                    Button {
                        camera.goBack(viewport: viewport, revision: revision)
                    } label: {
                        Label("Atlas Back", systemImage: "chevron.backward")
                    }
                    .disabled(!camera.canGoBack(revision: revision))
                    .keyboardShortcut("[", modifiers: .command)
                    .help("Return to the previous atlas location (⌘[)")
                    Button {
                        camera.goForward(viewport: viewport, revision: revision)
                    } label: {
                        Label("Atlas Forward", systemImage: "chevron.forward")
                    }
                    .disabled(!camera.canGoForward(revision: revision))
                    .keyboardShortcut("]", modifiers: .command)
                    .help("Revisit the next atlas location (⌘])")
                    Button {
                        guard camera.acceptsNavigation(revision: revision) else { return }
                        camera.steerZoom(by: 1 / 1.6, at: CGPoint(x: viewport.width / 2, y: viewport.height / 2))
                    } label: {
                        Image(systemName: "minus.magnifyingglass")
                    }
                    Text("\(String(format: camera.scale < 0.1 ? "%.2g" : "%.1f", camera.scale))×")
                        .font(.system(size: 11, design: .monospaced))
                        .monospacedDigit()
                    Button {
                        guard camera.acceptsNavigation(revision: revision) else { return }
                        camera.steerZoom(by: 1.6, at: CGPoint(x: viewport.width / 2, y: viewport.height / 2))
                    } label: {
                        Image(systemName: "plus.magnifyingglass")
                    }
                    Button {
                        camera.navigateFit(bounds: contentBounds, viewport: viewport, revision: revision)
                    } label: {
                        Image(systemName: "arrow.down.right.and.arrow.up.left")
                    }
                    .accessibilityLabel("Fit atlas")
                    .help("Fit all files in the atlas")
                }
                .buttonStyle(.bordered)
                .padding(10)
                .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
            }
            .overlay(alignment: .bottomTrailing) {
                VStack(alignment: .trailing, spacing: 4) {
                    Text("\(metadata.count) known files · \(scopeLabel)")
                        .font(.system(size: 11, design: .monospaced))
                    Text(residencySummary).font(.caption).lineLimit(2)
                    Button("Retry previews") {
                        guard camera.acceptsNavigation(revision: revision) else { return }
                        retryPreviews()
                        requestVisibleSource(in: size)
                    }.font(.caption)
                }
                .foregroundStyle(.secondary)
                .padding(6)
                .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 6))
                .padding(10)
            }
        }
    }

    /// Only the accepted display transform supplies demand. This callback
    /// records bounded scheduling input; source reads and shaping run later.
    private func requestVisibleSource(in size: CGSize) {
        guard active else { return }
        guard camera.acceptsNavigation(revision: revision),
              size.width.isFinite, size.height.isFinite, size.width > 0, size.height > 0,
              let origin = presentedFrame.worldPoint(at: .zero, revision: revision, contentRevision: contentRevision),
              let end = presentedFrame.worldPoint(at: CGPoint(x: size.width, y: size.height),
                  revision: revision, contentRevision: contentRevision), end.x > origin.x, end.y > origin.y else { return }
        let world = CGRect(x: origin.x, y: origin.y, width: end.x - origin.x, height: end.y - origin.y)
        let scale = size.width / world.width
        guard scale.isFinite, scale > 0 else { return }
        onViewport(world, scale)
    }
}

// MARK: - Hit row

struct HitRow: View {
    let hit: SearchHit
    let isSameFile: Bool
    let availability: AtlasSearchHitAvailability
    let openFile: (() -> Void)?

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: hit.structure.map { $0.mode == .headings ? "number" : "curlybraces" } ?? "doc.text")
                .foregroundStyle(isSameFile ? Color.accentColor : .secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(hit.structure?.name ?? hit.fileName)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(isSameFile ? Color.accentColor : .primary)
                if let structure = hit.structure {
                    Text(structure.label).font(.caption2).foregroundStyle(.secondary)
                    Text(hit.fileName).font(.caption).lineLimit(1).truncationMode(.middle)
                }
                Text(hit.folder)
                    .font(.system(size: 10.5))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let excerpt = hit.excerpt {
                    (Text(verbatim: excerpt.leading).foregroundColor(.secondary)
                        + Text(verbatim: excerpt.matched).bold().foregroundColor(.primary)
                        + Text(verbatim: excerpt.trailing).foregroundColor(.secondary))
                        .font(.system(size: 11, design: .monospaced))
                        .lineLimit(3)
                        .help(excerpt.hasReplacements
                            ? "Captured source context includes decoding replacements."
                            : "Captured source context; line breaks are shown as ↵ and tabs as ⇥.")
                }
                if !availability.label.isEmpty {
                    Text(availability.label).font(.caption2).foregroundStyle(.secondary)
                }
            }
            Spacer()
            if let openFile {
                Button("Open file", action: openFile)
                    .buttonStyle(.borderless)
                    .help("Read the file without using unverified match coordinates")
                    .accessibilityLabel("Open \(hit.fileName) without match navigation")
            } else {
                Text("b\(hit.start)")
                    .font(.system(size: 10.5, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, 1)
        .help(availability.message)
        .accessibilityHint(availability.message)
    }
}

// MARK: - Native scroll and magnification, in Canvas top-left coordinates

struct AtlasInputOverlay: NSViewRepresentable {
    let onZoom: (Double, CGPoint, Bool) -> Void
    let onWheel: (Double, CGPoint, Bool) -> Void

    final class Holder: NSView {
        var monitor: Any?
        var onZoom: (Double, CGPoint, Bool) -> Void = { _, _, _ in }
        var onWheel: (Double, CGPoint, Bool) -> Void = { _, _, _ in }
        override var isFlipped: Bool { true }
        override func hitTest(_ point: NSPoint) -> NSView? { nil }
        func stop() {
            if let monitor { NSEvent.removeMonitor(monitor) }
            monitor = nil
        }
        override func viewDidMoveToWindow() {
            stop()
            guard window != nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: [.scrollWheel, .magnify]) { [weak self] event in
                guard let self, event.window === self.window else { return event }
                let point = self.convert(event.locationInWindow, from: nil)
                guard self.bounds.contains(point) else { return event }
                if event.type == .magnify {
                    self.onZoom(exp(Double(event.magnification)), point, true)
                } else {
                    self.onWheel(Double(event.scrollingDeltaY), point, event.hasPreciseScrollingDeltas)
                }
                return nil
            }
        }
    }
    func makeNSView(context: Context) -> Holder {
        let view = Holder()
        view.onZoom = onZoom
        view.onWheel = onWheel
        return view
    }
    func updateNSView(_ view: Holder, context: Context) {
        view.onZoom = onZoom
        view.onWheel = onWheel
    }
    static func dismantleNSView(_ view: Holder, coordinator: ()) { view.stop() }
}

// MARK: - App

extension AtlasDeskApplicationDelegate {
    func application(_ application: NSApplication, open urls: [URL]) {
        AtlasNativeOpenRouting.shared.open(urls)
    }
}

@main
@MainActor struct FrankenCodeBrowserApp: App {
    @NSApplicationDelegateAdaptor(AtlasDeskApplicationDelegate.self) private var applicationDelegate
    @AppStorage("fcb.appearance") private var appearance = AtlasAppearancePreference.system.rawValue
    @AppStorage("fcb.increasedContrast") private var increasedContrast = false
    var body: some Scene {
        WindowGroup("FrankenCodeBrowser", id: "browser") {
            ContentView()
                .preferredColorScheme((AtlasAppearancePreference(rawValue: appearance) ?? .system).colorScheme)
        }
        .commands {
            AtlasNativeOpenCommands()
            CommandMenu("Appearance") {
                Picker("Appearance", selection: $appearance) {
                    ForEach(AtlasAppearancePreference.allCases) { preference in
                        Text(preference.title).tag(preference.rawValue)
                    }
                }
                Divider()
                Toggle("Increase Contrast", isOn: $increasedContrast)
                    .help("Increase syntax and selection contrast. The system Increase Contrast setting is always respected.")
            }
        }
    }
}
