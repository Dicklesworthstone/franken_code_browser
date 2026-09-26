// FrankenCodeBrowser — SwiftUI application shell over the real fcb engine.
//
// The main surface is the ATLAS: fcb-map's partition layout positions
// every file in the workspace. Every admitted text file displays its actual
// captured glyphs at overview and detail scales, independent of selection.
// Whole-source admission remains bounded; errors are visible. All data flows
// through the fcb-bridge staticlib into the same engine the `fcb` CLI
// uses.

import SwiftUI
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

struct AtlasFile: Identifiable, Hashable {
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

@MainActor struct ContentView: View {
#if FCB_APP_STORE
    @State private var root = ""
    @State private var rootAccess: AppStoreRootAccess?
#else
    @State private var root = UserDefaults.standard.string(forKey: "fcb.root")
        ?? Engine.defaultRoot
#endif
    @State private var query = ""
    @FocusState private var searchFocused: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var searchReport: AtlasSearchReport?
    @State private var searchPending = false
    // Created only on explicit submission, not on every SwiftUI view rebuild.
    @State private var searchCoordinator: AtlasSearchCoordinator?
    @State private var resolvedMatches: [SearchHit.ID: AtlasMatch] = [:]
    @State private var searchRows: [CGRect] = []
    @State private var searchPaths: Set<String> = []
    @State private var overlayLimited = false
    @State private var showsSidebar = true
    @State private var loadingProject = false
    @State private var loadProgress = ""
    @State private var loadGeneration = UUID()
    @State private var loadTask: Task<Void, Never>?
    @State private var showsReader = false
    @State private var hits: [SearchHit] = []
    @State private var selectedHit: SearchHit.ID?
    @State private var searchTitle = "Search project"
    @State private var searchSummary = "Enter text to search the project."
    @State private var files: [AtlasFile] = []
    @State private var fileScope: AtlasFileScope = .all
    @State private var customExtensions = ""
    @State private var displayedFiles: [AtlasFile] = []
    @State private var selectedPath: String?
    @State private var fileText = ""
    @State private var selectedSource: AtlasSource?
    @State private var atlasDocuments: [String: AtlasDocument] = [:]
    @State private var projectCache: AtlasProjectCache?
    @State private var textTiles: [AtlasTextTile] = []
    @State private var atlasRevision = UUID()
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

    var body: some View {
        HStack(spacing: 0) {
            atlasScreen
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            if showsSidebar {
                sidebar.frame(width: 300).background(.bar)
            }
        }
        .frame(minWidth: 1180, minHeight: 740)
        .toolbar {
            Button {
                showsSidebar = true
                searchFocused = true
            } label: {
                Label("Search and Projects", systemImage: "sidebar.right")
            }
            .keyboardShortcut("f", modifiers: .command)
            .help("Show search and projects (⌘F)")
            Menu {
                Picker("Files shown", selection: $fileScope) {
                    ForEach(AtlasFileScope.allCases) { scope in Text(scope.rawValue).tag(scope) }
                }
            } label: { Label(fileScope.rawValue, systemImage: "line.3.horizontal.decrease.circle") }
            .help("Filter the atlas by file type")
            Button {
                showsReader.toggle()
            } label: {
                Label("Source Reader", systemImage: "doc.text")
            }
            .disabled(selectedPath == nil)
            .help("Show or hide the selectable source reader")
        }
        .onAppear {
#if FCB_APP_STORE
            if let recent = Engine.recents.first { restoreProject(recent) }
            else { status = "Choose a project folder to begin exploring its source." }
#else
            if !root.isEmpty { requestAtlasLoad() }
#endif
        }
        .onDisappear {
            loadTask?.cancel()
            loadTask = nil
            searchCoordinator?.cancel()
            searchCoordinator = nil
            searchPending = false
        }
        .onChange(of: query) { _, _ in clearSearch() }
        .onChange(of: fileScope) { _, value in
            if value == .custom { showsSidebar = true }
            applyFileScope()
        }
        .onChange(of: selectedHit) { _, selected in
            guard let hit = hits.first(where: { $0.id == selected }) else { return }
            guard let path = hit.sourcePath else {
                status = "This filename cannot be opened by the UTF-8 reader. The search result is retained."
                return
            }
            openFile(path)
            showsReader = true
            if sourceError == nil {
                if let match = resolvedMatches[hit.id] {
                    selectedMatch = match
                    searchRows = Array((match.rowRects + searchRows.filter { !match.rowRects.contains($0) }).prefix(256))
                    status = "Matching source rows highlighted in the captured file."
                } else {
                    status = "Opened file beginning. Exact match location unavailable: the capture changed or its offsets cannot be mapped."
                }
            }
        }
    }

    // MARK: Sidebar

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
            HStack {
                TextField("Exact text in project…", text: $query)
                    .textFieldStyle(.roundedBorder)
                    .focused($searchFocused)
                    .onSubmit(runSearch)
                    .onExitCommand { query = ""; clearSearch() }
                Button("Search") { runSearch() }
                    .disabled(loadingProject || root.isEmpty || query.trimmingCharacters(in: .whitespaces).isEmpty)
                if !query.isEmpty {
                    Button { query = ""; clearSearch() } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Clear search")
                }
            }
            .padding(.horizontal, 10)
            .padding(.top, 10)
            if searchPending {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("Searching project")
                    Text("Searching…").font(.caption)
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
                let count = searchReport?.matchesSeen ?? 0
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(count, format: .number)
                        .font(.system(size: 30, weight: .semibold, design: .rounded))
                        .monospacedDigit()
                        .contentTransition(.numericText())
                        .animation(reduceMotion ? nil : .spring(response: 0.38, dampingFraction: 0.82), value: count)
                    Text(searchReport.map { $0.complete ? "workspace matches" : "workspace matches · partial" } ?? "ready to search")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                }
                .padding(10)
                if overlayLimited {
                    Text("Some match rows are unavailable or exceed the overlay limit. File outlines show returned matches.")
                        .font(.caption).foregroundStyle(.secondary).padding(.horizontal, 10)
                }
            }
            if hits.isEmpty {
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
                List(selection: $selectedHit) {
                    ForEach(hits) { hit in
                        HitRow(hit: hit, isSameFile: selectedPath == hit.sourcePath)
                            .tag(hit.id)
                    }
                }
                .listStyle(.sidebar)
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
                    Text(root.isEmpty ? "Choose a folder to begin" : "\(files.count) files · \(root)")
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

    private var atlasScreen: some View {
        HStack(spacing: 0) {
            VStack(spacing: 0) {
                if textTiles.isEmpty {
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
                    AtlasCanvas(
                        revision: atlasRevision,
                        focusRequest: focusRequest,
                        selectedMatch: selectedMatch,
                        searchRows: searchRows,
                        scopeLabel: fileScope == .custom ? customExtensions : fileScope.rawValue,
                        files: displayedFiles,
                        camera: camera,
                        hitPaths: searchPaths,
                        selectedPath: selectedPath,
                        contentBounds: contentBounds,
                        onSelect: { openFile($0.path) },
                        tiles: textTiles
                    )
                    .id(atlasRevision)
                }
                if selectedPath != nil && showsReader {
                    sourcePane.frame(height: 240)
                }
            }
            .frame(maxWidth: .infinity)

            if selectedPath != nil && showsReader {
                Divider()
                inspector.frame(width: 264)
            }
        }
    }

    private var inspector: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Inspector")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Color.accentColor)
            if let path = selectedPath, let file = files.first(where: { $0.path == path }) {
                Text(file.fileName + " · file")
                    .font(.system(size: 13, weight: .medium))
                Text(path)
                    .font(.system(size: 10.5, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                Divider()
                stat("bytes", ByteCountFormatter.string(fromByteCount: Int64(file.bytes), countStyle: .file))
                stat("profile rows", "\(file.lineCount)")
                stat("preview coverage", file.previewCoverage)
                stat("captured lines", selectedSource.map { "\($0.lines.count)" } ?? "—")
                let fileHits = hits.filter { $0.sourcePath == path }
                stat("matches", "\(fileHits.count)")
                stat("project files", "\(files.count)")
            } else {
                Text("Click a file cell to inspect it.")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                stat("project files", "\(files.count)")
                stat("project", (root as NSString).lastPathComponent)
            }
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
            if let path = selectedPath {
                HStack(spacing: 8) {
                    Image(systemName: "doc.text")
                        .foregroundStyle(.secondary)
                    Text(path)
                        .font(.system(size: 11.5, design: .monospaced))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    let fileHits = hits.filter { $0.sourcePath == path }
                    Text("\(fileHits.count) match\(fileHits.count == 1 ? "" : "es") in file")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(.bar)
                Divider()
            }
            if let source = selectedSource, sourceError == nil {
                AtlasSourceReader(source: source, navigation: focusRequest,
                    selection: selectedMatch.map { AtlasReaderSelection(source: source, range: $0.sourceRange) }) {
                    if let document = atlasDocuments[source.path] {
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
        .background(Color(red: 0.153, green: 0.157, blue: 0.133))
    }

    // MARK: Actions

    private func requestAtlasLoad() {
        loadTask?.cancel()
        loadGeneration = UUID()
        let generation = loadGeneration
#if !FCB_APP_STORE
        if let argRoot = CommandLine.arguments.dropFirst().first,
            FileManager.default.fileExists(atPath: argRoot)
        {
            root = argRoot
            UserDefaults.standard.set(argRoot, forKey: "fcb.root")
            Engine.remember(root: argRoot)
        }
#endif
        let requestedRoot = root
#if FCB_APP_STORE
        let accessLease = rootAccess
#endif
        files = []
        displayedFiles = []
        atlasDocuments = [:]
        textTiles = []
        loadingProject = true
        loadProgress = "Discovering files…"
        loadTask = Task {
#if FCB_APP_STORE
            // Retain the security-scoped grant until this load stops using Rust.
            defer { withExtendedLifetime(accessLease) {} }
#endif
            await loadAtlas(root: requestedRoot, generation: generation)
        }
    }

    private func cancelAtlasLoad() {
        loadTask?.cancel()
        loadTask = nil
        loadGeneration = UUID()
        loadingProject = false
        loadProgress = ""
        status = "Project loading canceled. Choose a folder to try again."
    }

    private func loadAtlas(root requestedRoot: String, generation: UUID) async {
        defer {
            if loadGeneration == generation {
                loadingProject = false
                loadTask = nil
            }
        }
        // A refresh changes the accepted atlas even when the path is unchanged.
        clearSearch()
#if FCB_APP_STORE
        guard rootAccess?.url.path == requestedRoot else {
            status = "Choose a project folder to grant read access."
            return
        }
#endif
        let atlasFiles = Engine.atlas(root: requestedRoot)
        guard !Task.isCancelled, loadGeneration == generation else { return }
        files = atlasFiles
        loadProgress = "Preparing source previews for \(atlasFiles.count) files…"
        await Task.yield()
        guard !Task.isCancelled, loadGeneration == generation else { return }
        if projectCache?.root != requestedRoot {
            projectCache = AtlasProjectCache.defaultDirectory(root: requestedRoot).map {
                AtlasProjectCache(root: requestedRoot, cacheDirectory: $0)
            }
        }
        let cache = projectCache
        cache?.beginRefresh()
        var documents: [String: AtlasDocument] = [:]
        // Keep the eager working set proportional to RAM. Capture source before
        // generated evidence so a broad repository does not spend the entire
        // first-open budget on its first alphabetic artifact directory.
        var remainingBytes = Int(min(UInt64(512 * 1024 * 1024),
            max(UInt64(64 * 1024 * 1024), ProcessInfo.processInfo.physicalMemory / 128)))
        var tiles: [AtlasTextTile] = []
        let captureOrder = atlasFiles.sorted { left, right in
            let leftRank = Self.captureRank(left.path)
            let rightRank = Self.captureRank(right.path)
            return leftRank == rightRank ? left.path < right.path : leftRank < rightRank
        }
        for (index, file) in captureOrder.enumerated() {
            guard !Task.isCancelled, loadGeneration == generation else { return }
            guard file.bytes <= remainingBytes, file.bytes <= 4 * 1024 * 1024 else { continue }
            let prepared: AtlasDocument?
            if let cache {
                prepared = cache.document(path: file.path) { Engine.sourceCapture(root: requestedRoot, path: file.path) }
            } else {
                prepared = Engine.sourceCapture(root: requestedRoot, path: file.path).flatMap { AtlasDocument(path: file.path, capture: $0) }
            }
            guard let document = prepared,
                  document.source.text.utf8.count <= remainingBytes,
                  document.tiles.count <= 65536 - tiles.count else { continue }
            remainingBytes -= document.source.text.utf8.count
            documents[file.path] = document
            tiles.append(contentsOf: document.tiles)
            if index.isMultiple(of: 8) {
                loadProgress = "Prepared \(index + 1) of \(captureOrder.count) files…"
                await Task.yield()
            }
        }
        guard !Task.isCancelled, loadGeneration == generation else { return }
        loadProgress = "Laying out the atlas…"
        await Task.yield()
        guard !Task.isCancelled, loadGeneration == generation else { return }
        atlasDocuments = documents
        displayedFiles = atlasFiles
        if let bounds = Engine.placeTextTiles(tiles), let displayTiles = AtlasParcelLayout.reflow(documents) {
            loadProgress = "Finishing source previews…"
            await Task.yield()
            guard !Task.isCancelled, loadGeneration == generation else { return }
            cache?.finishRefresh(documents: documents)
            await Task.yield()
            guard !Task.isCancelled, loadGeneration == generation else { return }
            cache?.prepareOverview(documents: documents)
            guard !Task.isCancelled, loadGeneration == generation else { return }
            // A disabled disk cache still renders complete balanced columns.
            if cache == nil {
                let budget = max(1, 64 * 1024 * 1024 / max(1, displayTiles.count))
                for tile in displayTiles { tile.prepareRaster(pixelBudget: budget) }
            }
            textTiles = displayTiles
            atlasRevision = UUID()
            contentBounds = bounds
            camera.contentBounds = bounds
        } else {
            textTiles = []
            atlasRevision = UUID()
            status = "Text layout unavailable for this project."
        }
        selectedPath = nil
        selectedMatch = nil
        fileText = ""
        selectedSource = nil
        sourceError = nil
        if atlasFiles.isEmpty {
            let planJSON = Engine.plan(root: requestedRoot)
            status = planJSON.count > 2
                ? "Atlas unavailable for this root — the engine's bounded discovery refused it (\(planJSON.count)-byte plan). Pick a smaller subdirectory."
                : "No atlas for \(requestedRoot): empty, unreadable, or beyond walk bounds."
        } else if textTiles.isEmpty {
            status = "Source text layout unavailable. \(atlasDocuments.count) files captured."
        } else {
            status = "\(atlasFiles.count) files laid out. Scroll to zoom, drag to pan, click a file. Source loaded for \(atlasDocuments.count) files."
        }
        if fileScope != .all { applyFileScope() }
    }

    private static func captureRank(_ path: String) -> Int {
        let name = (path as NSString).lastPathComponent
        let ext = (name as NSString).pathExtension.lowercased()
        if ["rs", "swift", "py", "go", "c", "h", "cpp", "hpp", "m", "mm", "js", "jsx", "ts", "tsx", "java", "kt", "sh", "bash", "zig"].contains(ext) { return 0 }
        if ["md", "mdx", "rst", "txt"].contains(ext) { return 1 }
        if ["toml", "yaml", "yml", "xml", "html", "css", "sql"].contains(ext) { return 2 }
        if path.hasPrefix(".beads/") || path.hasPrefix("artifacts/") || path.hasPrefix(".rch-out/") { return 4 }
        return 3
    }

    private func applyFileScope() {
        clearSearch()
        selectedPath = nil; selectedSource = nil; selectedMatch = nil
        let scoped = files.filter { fileScope.includes($0.path, custom: customExtensions) }
        let paths = Set(scoped.map(\.path))
        let documents = atlasDocuments.filter { paths.contains($0.key) }
        let tiles = scoped.flatMap { documents[$0.path]?.tiles ?? [] }
        if let bounds = Engine.placeTextTiles(tiles), let display = AtlasParcelLayout.reflow(documents) {
            let budget = max(1, 64 * 1024 * 1024 / max(1, display.count))
            for tile in display where tile.raster == nil { tile.prepareRaster(pixelBudget: budget) }
            textTiles = display; contentBounds = bounds; camera.contentBounds = bounds
        } else { textTiles = [] }
        displayedFiles = scoped
        atlasRevision = UUID()
        status = "\(scoped.count) of \(files.count) files · \(fileScope.rawValue). Search still covers the workspace."
    }

    private func clearSearch() {
        searchCoordinator?.cancel()
        searchPending = false
        hits = []; selectedHit = nil; selectedMatch = nil
        searchReport = nil; resolvedMatches = [:]; searchRows = []; searchPaths = []
        overlayLimited = false
        searchTitle = "Search project"
        searchSummary = "Enter text to search the project."
        status = "\(displayedFiles.count) of \(files.count) files shown. Search cleared."
    }

    private func prepareSearchOverlay(_ report: AtlasSearchReport) {
        var matches: [SearchHit.ID: AtlasMatch] = [:]
        let grouped = Dictionary(grouping: report.hits.compactMap { hit in
            hit.sourcePath.map { ($0, hit) }
        }, by: { $0.0 })
        for path in grouped.keys.sorted() {
            guard fileScope.includes(path, custom: customExtensions), let document = atlasDocuments[path], let entries = grouped[path] else { continue }
            // A single workspace query names one capture per file. Reject any
            // inconsistent identity rather than borrowing another hit's proof.
            guard let first = entries.first?.1, let digest = first.captureSHA256,
                  let count = first.captureByteLength else { continue }
            let valid = entries.map(\.1).filter { $0.captureSHA256 == digest && $0.captureByteLength == count }
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
        searchPaths = Set(report.hits.compactMap(\.sourcePath))
        overlayLimited = limited
    }

    private func runSearch() {
        let requestedText = query
        let trimmed = requestedText.trimmingCharacters(in: .whitespaces)
        let requestedRoot = root
        guard !trimmed.isEmpty, !requestedRoot.isEmpty else { return }
#if FCB_APP_STORE
        guard let access = rootAccess, access.url.path == requestedRoot else {
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
            try AtlasSearchCoordinator.validate(root: requestedRoot, query: trimmed)
            clearSearch()
            searchPending = true
            searchTitle = "Searching project"
            searchSummary = "Searching captured source text. You can navigate the atlas or cancel."
            status = searchSummary
            let coordinator = searchCoordinator ?? AtlasSearchCoordinator(work: AtlasNativeSearch.run)
            searchCoordinator = coordinator
            try coordinator.submit(root: requestedRoot, query: trimmed, accessLease: accessLease) { result in
                // Generation rejection happens in the coordinator. These checks
                // also cover SwiftUI state changes before onChange has run.
                guard root == requestedRoot, query == requestedText else { return }
                searchPending = false
                switch result {
                case .success(let report):
                    prepareSearchOverlay(report)
                    searchReport = report
                    hits = report.hits.filter { $0.sourcePath.map { fileScope.includes($0, custom: customExtensions) } ?? (fileScope == .all) }
                    status = report.summary
                    searchSummary = report.summary
                    searchTitle = report.complete ? "No matches" : "Partial search"
                case .failure(let error):
                    status = error.message
                    searchSummary = status
                    if case .canceled = error { searchTitle = "Search canceled" }
                    else { searchTitle = "Search unavailable" }
                }
            }
        } catch {
            searchPending = false
            status = (error as? AtlasSearchError)?.message ?? "Search failed. Results are unavailable."
            searchSummary = status
            searchTitle = "Search unavailable"
        }
    }

    private func cancelSearch() {
        searchCoordinator?.cancel()
        searchPending = false
        searchTitle = "Search canceled"
        searchSummary = AtlasSearchError.canceled.message
        status = searchSummary
    }

    private func openFile(_ path: String) {
        selectedMatch = nil
        selectedPath = path
        focusRequest = UUID()
        if let text = atlasDocuments[path]?.source.text ?? Engine.read(root: root, path: path) {
            fileText = text
            selectedSource = atlasDocuments[path]?.source ?? AtlasSource(path: path, text: text)
            sourceError = nil
            status = "Opened \(path). Zoom into its parcel to read the captured source."
        } else {
            fileText = ""
            selectedSource = nil
            sourceError = "Source unavailable: this reader accepts UTF-8 files up to 4 MiB, without embedded NUL. The file may also have changed or become inaccessible."
            status = sourceError ?? "Source unavailable"
        }
    }

#if FCB_APP_STORE
    private func activateProject(_ access: AppStoreRootAccess) {
        rootAccess = access
        root = access.url.path
        clearSearch()
        searchTitle = "Search project"
        searchSummary = "Enter text to search the project."
        selectedPath = nil
        requestAtlasLoad()
    }

    private func restoreProject(_ path: String) {
        guard let access = AppStoreRootAccess.restore(path) else {
            status = "Saved project access is unavailable. Choose the folder again to restore it."
            return
        }
        activateProject(access)
    }
#else
    private func setRoot(_ newRoot: String) {
        root = newRoot
        UserDefaults.standard.set(newRoot, forKey: "fcb.root")
        Engine.remember(root: newRoot)
        clearSearch()
        searchTitle = "Search project"
        searchSummary = "Enter text to search the project."
        selectedPath = nil
        requestAtlasLoad()
    }
#endif

    private func chooseProject() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.directoryURL = root.isEmpty ? FileManager.default.homeDirectoryForCurrentUser : URL(fileURLWithPath: root)
        panel.message = "Choose the project root to lay out and browse"
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
#if FCB_APP_STORE
            guard let access = AppStoreRootAccess.select(url) else {
                status = "Could not retain read access to this folder. Choose it again."
                return
            }
            activateProject(access)
#else
            setRoot(url.path)
#endif
        }
    }
}

// MARK: - Atlas canvas (display-link driven, text at every zoom)

struct AtlasCanvas: View {
    let revision: UUID
    let focusRequest: UUID
    let selectedMatch: AtlasMatch?
    let searchRows: [CGRect]
    let scopeLabel: String
    let files: [AtlasFile]
    let camera: AtlasCamera
    let hitPaths: Set<String>
    let selectedPath: String?
    let contentBounds: CGRect
    let onSelect: (AtlasFile) -> Void
    let tiles: [AtlasTextTile]

    @GestureState private var dragDelta: CGSize = .zero
    @State private var viewport: CGSize = .zero
    @State private var initialFitPending = true
    @State private var presentedFrame = AtlasPresentedFrame()
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        GeometryReader { geometry in
            let size = geometry.size
            TimelineView(.animation(paused: !camera.isAnimating)) { timeline in
                AtlasRetainedView(tiles: tiles, revision: revision, scale: camera.scale,
                    offset: CGPoint(x: camera.offsetX, y: camera.offsetY),
                    selectedPath: selectedPath, hitPaths: hitPaths, matchRows: searchRows.isEmpty ? (selectedMatch?.rowRects ?? []) : searchRows,
                    onPresentedFrame: { revision, scale, offset in
                        presentedFrame.record(revision: revision, scale: scale, offset: offset)
                    })
                .onChange(of: timeline.date) { _, _ in
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
                        camera.drag(location: value.location, start: value.startLocation,
                                    now: ProcessInfo.processInfo.systemUptime)
                    }
                    .onEnded { _ in
                        camera.endPan(now: ProcessInfo.processInfo.systemUptime)
                    }
            )
            .simultaneousGesture(
                SpatialTapGesture(count: 2)
                    .onEnded { value in
                        camera.steerZoom(by: 2.2, at: value.location)
                    }
            )
            .simultaneousGesture(
                SpatialTapGesture()
                    .onEnded { value in
                        guard dragDelta == .zero,
                              let point = presentedFrame.worldPoint(at: value.location, revision: revision),
                              let tile = tiles.last(where: { ($0.parcelRect ?? $0.rect).contains(point) }),
                              let file = files.first(where: { $0.path == tile.path }) else {
                            return
                        }
                        onSelect(file)
                    }
            )
            .overlay {
                AtlasInputOverlay(onZoom: { factor, point, immediate in
                    camera.steerZoom(by: factor, at: point, immediate: immediate)
                }, onWheel: { delta, point, precise in
                    camera.steerWheelZoom(delta: delta, precise: precise, at: point)
                })
            }
            .onAppear {
                camera.reducedMotion = reduceMotion
            }
            .onChange(of: reduceMotion) { _, value in camera.reducedMotion = value }
            .onChange(of: focusRequest) { _, _ in
                guard let path = selectedPath, let first = tiles.first(where: { $0.path == path }) else { return }
                camera.focus(bounds: selectedMatch?.focusRect ?? first.rect, viewport: size)
            }
            .onChange(of: size, initial: true) { _, newSize in
                viewport = newSize
                camera.updateViewport(newSize)
                // onAppear can precede the first usable GeometryReader size.
                // Retry until fit succeeds, then leave navigation under user control.
                if initialFitPending {
                    initialFitPending = !camera.fit(bounds: contentBounds, viewport: newSize)
                }
            }
            .overlay(alignment: .bottomLeading) {
                HStack(spacing: 8) {
                    Button {
                        camera.steerZoom(by: 1 / 1.6, at: CGPoint(x: viewport.width / 2, y: viewport.height / 2))
                    } label: {
                        Image(systemName: "minus.magnifyingglass")
                    }
                    Text("\(String(format: camera.scale < 0.1 ? "%.2g" : "%.1f", camera.scale))×")
                        .font(.system(size: 11, design: .monospaced))
                        .monospacedDigit()
                    Button {
                        camera.steerZoom(by: 1.6, at: CGPoint(x: viewport.width / 2, y: viewport.height / 2))
                    } label: {
                        Image(systemName: "plus.magnifyingglass")
                    }
                    Button {
                        camera.fit(bounds: contentBounds, viewport: viewport)
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
                Text("\(files.count) files · \(scopeLabel)")
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .padding(6)
                    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 6))
                    .padding(10)
            }
        }
    }


}

// MARK: - Hit row

struct HitRow: View {
    let hit: SearchHit
    let isSameFile: Bool

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "doc.text")
                .foregroundStyle(isSameFile ? Color.accentColor : .secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(hit.fileName)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(isSameFile ? Color.accentColor : .primary)
                Text(hit.folder)
                    .font(.system(size: 10.5))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer()
            Text("b\(hit.start)")
                .font(.system(size: 10.5, design: .monospaced))
                .foregroundStyle(.tertiary)
        }
        .padding(.vertical, 1)
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

@main
struct FrankenCodeBrowserApp: App {
    var body: some Scene {
        WindowGroup("FrankenCodeBrowser") {
            ContentView()
                .preferredColorScheme(.dark)
        }
    }
}
