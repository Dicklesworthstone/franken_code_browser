import Foundation

@_silgen_name("fcb_search_workspace_cancelable")
private func searchWorkspaceCancelable(
    _ root: UnsafePointer<CChar>?, _ query: UnsafePointer<CChar>?,
    _ poll: (@convention(c) (UnsafeMutableRawPointer?) -> Int32)?,
    _ context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<CChar>?

@_silgen_name("fcb_free_string")
private func releaseSearchString(_ pointer: UnsafeMutablePointer<CChar>?)

/// The production worker adapter. Strings and cancellation storage remain alive
/// throughout the synchronous FFI call. JSON is decoded off the main thread;
/// only the bounded report crosses back to the native UI. No private source
/// parser, directory traversal, query engine or global cancellation registry.
enum AtlasNativeSearch {
    static func run(root: String, query: String,
                    cancellation: AtlasSearchCancellation) throws -> AtlasSearchReport {
        try AtlasSearchCoordinator.validate(root: root, query: query)
        if cancellation.isCanceled { throw AtlasSearchError.canceled }
        let poll: @convention(c) (UnsafeMutableRawPointer?) -> Int32 = { context in
            guard let context else { return 1 }
            let flag = Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue()
            return flag.isCanceled ? 1 : 0
        }
        let pointer = withExtendedLifetime(cancellation) {
            root.withCString { root in
                query.withCString { query in
                    searchWorkspaceCancelable(root, query, poll,
                        Unmanaged.passUnretained(cancellation).toOpaque())
                }
            }
        }
        // Release even when cancellation races a successful returned response.
        defer { releaseSearchString(pointer) }
        if cancellation.isCanceled { throw AtlasSearchError.canceled }
        guard let pointer else { throw AtlasSearchError.unavailable }
        let report = try AtlasSearchReport.decode(String(cString: pointer))
        if cancellation.isCanceled { throw AtlasSearchError.canceled }
        return report
    }
}
