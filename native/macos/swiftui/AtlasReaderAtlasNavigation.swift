import Foundation

/// An explicit locator for the current project, derived from an accepted reader
/// visit. It does not transfer capture bytes or make the current atlas evidence
/// for that capture. The host must still authorize and discover the destination.
struct AtlasReaderAtlasNavigation: Sendable {
    private let project: UUID
    private let root: [UInt8]
    private let grant: ObjectIdentifier?
    private let browser: ObjectIdentifier
    private let visit: UUID
    private let activation: UUID
    private let relativePath: [UInt8]
    private let identity: AtlasReaderIdentity

    let file: AtlasFileFocus?
    let directory: AtlasDirectoryFocus?

    init?(project: UUID, root: String, grant: ObjectIdentifier?, browser: ObjectIdentifier,
          visit: UUID, activation: UUID, captureRoot: String, relativePath: [UInt8],
          identity: AtlasReaderIdentity) {
        guard root.utf8.first == 47, root.utf8.count <= 16_384, !root.utf8.contains(0),
              captureRoot.utf8.elementsEqual(root.utf8),
              !relativePath.isEmpty, relativePath.count <= 16_384,
              relativePath.first != 47, !relativePath.contains(0),
              !relativePath.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                  $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
              }), identity.owner > 0, identity.file > 0, identity.revision > 0,
              identity.pathBytes.count <= 16_384 else { return nil }
        let rootBytes = Array(root.utf8)
        // Retained imports name a relative path; an ordinary live open names
        // the absolute path. Neither a prefix nor an escaped label is enough.
        if identity.pathBytes != relativePath {
            let absolute = rootBytes + (rootBytes.last == 47 ? [] : [47]) + relativePath
            guard identity.pathBytes == absolute else { return nil }
        }
        self.project = project; self.root = rootBytes; self.grant = grant; self.browser = browser
        self.visit = visit; self.activation = activation
        self.relativePath = relativePath; self.identity = identity
        file = try? AtlasFileFocus(rawPath: relativePath)
        // Rust's atlas_session::directory_path and the progressive catalog
        // share 31 directory components; file_path admits a 32nd file component.
        if let directory = try? AtlasDirectoryFocus.containingFile(rawPath: relativePath),
           directory.rawPath.split(separator: 47).count < AtlasFileFocus.maximumComponents {
            self.directory = directory
        } else { self.directory = nil }
    }

    /// Quick Open and source links are independent of the atlas's type filter.
    /// An explicit locate includes the selected file; the navigation transaction
    /// keeps the previous filter and camera as its Back destination.
    var fileDestination: AtlasFolderDestination? {
        guard let file else { return nil }
        return try? AtlasFolderDestination(catalogTarget: .file(file), fileScope: .all, customExtensions: "")
    }
    var directoryDestination: AtlasFolderDestination? {
        guard let directory else { return nil }
        return try? AtlasFolderDestination(directory: directory, fileScope: .all, customExtensions: "")
    }

    /// Rebuild the current value after every asynchronous boundary. Returning
    /// to the same retained capture is a new activation, so an old queued menu
    /// action cannot revive after Source Back/Forward or a project replacement.
    func isCurrent(_ current: Self?) -> Bool {
        guard let current else { return false }
        return project == current.project && root == current.root && grant == current.grant
            && browser == current.browser && visit == current.visit && activation == current.activation
            && relativePath == current.relativePath && identity.matches(current.identity)
    }
}
