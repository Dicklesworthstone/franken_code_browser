import Foundation

#if FCB_APP_STORE
/// Keeps a user-selected directory's sandbox extension alive while the Rust bridge reads it.
/// Persisted paths are labels only; reopening requires a valid security-scoped bookmark.
final class AppStoreRootAccess {
    private static let recentPathsKey = "fcb.appStore.recentPaths"
    private static let bookmarksKey = "fcb.appStore.rootBookmarks"
    private static let maxRecent = 6

    let url: URL

    private init?(url: URL) {
        guard url.startAccessingSecurityScopedResource() else { return nil }
        self.url = url
    }

    deinit { url.stopAccessingSecurityScopedResource() }

    static var recents: [String] {
        let bookmarks = storedBookmarks
        return (UserDefaults.standard.stringArray(forKey: recentPathsKey) ?? [])
            .filter { bookmarks[$0] != nil }
    }

    static func select(_ url: URL) -> AppStoreRootAccess? {
        guard let bookmark = try? url.bookmarkData(
            options: [.withSecurityScope, .securityScopeAllowOnlyReadAccess],
            includingResourceValuesForKeys: nil, relativeTo: nil)
        else { return nil }
        return open(bookmark: bookmark)
    }

    static func restore(_ path: String) -> AppStoreRootAccess? {
        guard let bookmark = storedBookmarks[path] else { return nil }
        return open(bookmark: bookmark)
    }

    private static func open(bookmark: Data) -> AppStoreRootAccess? {
        var stale = false
        guard let url = try? URL(resolvingBookmarkData: bookmark,
            options: [.withSecurityScope], relativeTo: nil, bookmarkDataIsStale: &stale),
            let access = AppStoreRootAccess(url: url)
        else { return nil }
        let currentBookmark: Data
        if stale {
            guard let refreshed = try? url.bookmarkData(
                options: [.withSecurityScope, .securityScopeAllowOnlyReadAccess],
                includingResourceValuesForKeys: nil, relativeTo: nil)
            else { return nil }
            currentBookmark = refreshed
        } else {
            currentBookmark = bookmark
        }
        remember(path: url.path, bookmark: currentBookmark)
        return access
    }

    private static var storedBookmarks: [String: Data] {
        UserDefaults.standard.dictionary(forKey: bookmarksKey) as? [String: Data] ?? [:]
    }

    private static func remember(path: String, bookmark: Data) {
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
