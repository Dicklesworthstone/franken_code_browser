import Foundation

extension AtlasProjectBrowserModel {
    /// Explicit current-source authority for a displayed catalog row. The host
    /// supplies its current project identity and grant, and revokes the observed
    /// presentation when that authority changes. An old capture's label cannot
    /// enter this route, even when its pathname happens to match the new root.
    /// Source opening and range I/O remain in AtlasObservedReaderModel's worker.
    func observedSource(_ file: AtlasProjectBrowserFile, from page: AtlasProjectBrowserPage,
        root currentRoot: String, project: UUID, browserOwner: UInt64,
        accessLease: AnyObject?) throws -> AtlasObservedReaderSource {
        guard root.utf8.elementsEqual(currentRoot.utf8), info?.owner == browserOwner,
              rootAccessIdentity == accessLease.map(ObjectIdentifier.init),
              let accepted = currentFile(file, from: page) else { throw AtlasProjectBrowserError.stale }
        let input = try AtlasObservedReaderInput(root: currentRoot, relativePath: accepted.rawPath)
        return AtlasObservedReaderSource(input: input, project: project, access: accessLease)
    }
}
