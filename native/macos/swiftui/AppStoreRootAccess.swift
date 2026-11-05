import Foundation

#if FCB_APP_STORE
private final class AppStoreRootScope: Sendable {
    let url: URL
    init?(url: URL) {
        guard url.startAccessingSecurityScopedResource() else { return nil }
        self.url = url
    }
    deinit { url.stopAccessingSecurityScopedResource() }
}

/// NSOpenPanel has already started access to its selected URL. Adopt that
/// access until bookmark creation has drained, then balance it once.
private final class AppStoreRootSelection {
    let url: URL
    init(url: URL) { self.url = url }
    deinit { url.stopAccessingSecurityScopedResource() }
}

/// Keeps a user-selected directory's sandbox extension alive while the Rust bridge reads it.
/// Persisted paths are labels only; reopening requires a valid security-scoped bookmark.
/// Immutable URL/bookmark data can cross the native worker boundary. A
/// successful start is balanced only when every consumer releases this lease.
final class AppStoreRootAccess: Sendable {
    private static let recentPathsKey = "fcb.appStore.recentPaths"
    private static let bookmarksKey = "fcb.appStore.rootBookmarks"
    private static let maxRecent = 6

    let url: URL
    private let bookmark: Data
    private let scope: AppStoreRootScope

    private init(scope: AppStoreRootScope, bookmark: Data) {
        self.url = scope.url
        self.scope = scope
        self.bookmark = bookmark
    }

    static var recents: [String] {
        let bookmarks = storedBookmarks
        return (UserDefaults.standard.stringArray(forKey: recentPathsKey) ?? [])
            .filter { bookmarks[$0] != nil }
    }

    static func restorationRequest(_ path: String) -> AtlasRootAccessRequest? {
        guard let bookmark = storedBookmarks[path] else { return nil }
        let request = AtlasRootAccessRequest.restored(bookmark)
        return request.isValid ? request : nil
    }

    static func selectionRequest(_ url: URL) -> (AtlasRootAccessRequest, AnyObject)? {
        let access = AppStoreRootSelection(url: url)
        let request = AtlasRootAccessRequest.selected(url)
        guard request.isValid else { return nil }
        return (request, access)
    }

    /// Runs only on the native-host worker. Resolving a missing volume must not
    /// mount it or present implicit UI while the app is restoring a saved root.
    static func resolve(_ request: AtlasRootAccessRequest, cancellation: AtlasSearchCancellation) throws -> AppStoreRootAccess {
        guard request.isValid else { throw AtlasRootAccessError.invalidRequest }
        try check(cancellation)
        let bookmark: Data
        switch request {
        case .restored(let stored): bookmark = stored
        case .selected(let url):
            bookmark = try url.bookmarkData(
                options: [.withSecurityScope, .securityScopeAllowOnlyReadAccess],
                includingResourceValuesForKeys: nil, relativeTo: nil)
        }
        try check(cancellation)
        guard bookmark.count <= AtlasRootAccessRequest.maximumBookmarkBytes else { throw AtlasRootAccessError.invalidRequest }
        var stale = false
        guard let url = try? URL(resolvingBookmarkData: bookmark,
            options: [.withSecurityScope, .withoutUI, .withoutMounting], relativeTo: nil, bookmarkDataIsStale: &stale),
            AtlasRootAccessRequest.selected(url).isValid else { throw AtlasRootAccessError.unavailable }
        try check(cancellation)
        guard let scope = AppStoreRootScope(url: url) else { throw AtlasRootAccessError.unavailable }
        try check(cancellation)
        guard try url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory == true else {
            throw AtlasRootAccessError.unavailable
        }
        try check(cancellation)
        let currentBookmark: Data
        if stale {
            guard let refreshed = try? url.bookmarkData(
                options: [.withSecurityScope, .securityScopeAllowOnlyReadAccess],
                includingResourceValuesForKeys: nil, relativeTo: nil)
            else { throw AtlasRootAccessError.unavailable }
            currentBookmark = refreshed
        } else {
            currentBookmark = bookmark
        }
        try check(cancellation)
        guard !currentBookmark.isEmpty, currentBookmark.count <= AtlasRootAccessRequest.maximumBookmarkBytes else {
            throw AtlasRootAccessError.invalidRequest
        }
        // Transfer the one acquired scope, including a refreshed bookmark.
        // No defaults are mutated by abandoned work.
        return AppStoreRootAccess(scope: scope, bookmark: currentBookmark)
    }

    @MainActor func remember() { Self.remember(path: url.path, bookmark: bookmark) }
    private static func check(_ cancellation: AtlasSearchCancellation) throws {
        if cancellation.isCanceled { throw AtlasProjectIOError.canceled }
    }

    private static var storedBookmarks: [String: Data] {
        UserDefaults.standard.dictionary(forKey: bookmarksKey) as? [String: Data] ?? [:]
    }

    @MainActor private static func remember(path: String, bookmark: Data) {
        let paths = [path] + recents.filter { $0 != path }
        let kept = Array(paths.prefix(maxRecent))
        var bookmarks = storedBookmarks
        bookmarks[path] = bookmark
        bookmarks = bookmarks.filter { kept.contains($0.key) }
        UserDefaults.standard.set(bookmarks, forKey: bookmarksKey)
        UserDefaults.standard.set(kept, forKey: recentPathsKey)
    }
}
#endif
