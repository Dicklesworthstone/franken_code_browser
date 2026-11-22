import Foundation

/// A place in the current project session. Identity is the original root-relative
/// directory and optional parcel path, never a response-local ID or source capture.
/// The owner binds this value to its current root/grant and discovery policy.
struct AtlasFolderDestination: Equatable {
    enum ValidationError: Error { case invalidScope, invalidAnchor }
    let directory: AtlasDirectoryFocus
    let fileScope: AtlasFileScope
    let customExtensions: String
    let anchorRawPath: [UInt8]?
    let cameraAnchor: AtlasParcelCameraAnchor?

    init(directory: AtlasDirectoryFocus, fileScope: AtlasFileScope, customExtensions: String,
        anchorRawPath: [UInt8]? = nil, cameraAnchor: AtlasParcelCameraAnchor? = nil) throws {
        guard customExtensions.utf8.count <= 1024,
              (try? fileScope.searchScope(custom: customExtensions)) != nil else {
            throw ValidationError.invalidScope
        }
        guard (anchorRawPath == nil) == (cameraAnchor == nil),
              anchorRawPath.map({ directory.containsFile($0) }) != false else {
            throw ValidationError.invalidAnchor
        }
        self.directory = directory; self.fileScope = fileScope
        self.customExtensions = customExtensions
        self.anchorRawPath = anchorRawPath; self.cameraAnchor = cameraAnchor
    }

    func matchesScope(fileScope: AtlasFileScope, customExtensions: String) -> Bool {
        self.fileScope == fileScope && self.customExtensions.utf8.elementsEqual(customExtensions.utf8)
    }

    static func == (left: Self, right: Self) -> Bool {
        left.directory == right.directory
            && left.matchesScope(fileScope: right.fileScope, customExtensions: right.customExtensions)
            && left.anchorRawPath == right.anchorRawPath && left.cameraAnchor == right.cameraAnchor
    }
}

/// Session-only semantic navigation. Preparing a move stages the latest camera
/// origin in a copy; canceled/failed work cannot consume Back or discard Forward.
/// Only the owner of the accepted root, catalog and layout may commit a proposal.
struct AtlasFolderNavigation {
    struct Move {
        let target: AtlasFolderDestination
        fileprivate let revision: UUID
        fileprivate let staged: AtlasNavigationHistory<AtlasFolderDestination>
        fileprivate let proposal: AtlasNavigationHistory<AtlasFolderDestination>.Move
    }
    struct Landmark: Identifiable, Equatable {
        let id: UUID
        let label: String
        let destination: AtlasFolderDestination
    }
    enum LandmarkError: Error { case invalidLabel, limit, missing }
    static let maximumLandmarks = 32
    static let maximumLabelBytes = 128

    private var history: AtlasNavigationHistory<AtlasFolderDestination>
    private var revision = UUID()
    private let landmarkCapacity: Int
    private(set) var landmarks: [Landmark] = []
    var current: AtlasFolderDestination? { history.current }
    var canBack: Bool { history.canBack }
    var canForward: Bool { history.canForward }
    var retainedCount: Int { history.retainedCount }

    init(historyCapacity: Int = 128, landmarkCapacity: Int = 32) {
        history = AtlasNavigationHistory(capacity: historyCapacity)
        self.landmarkCapacity = min(Self.maximumLandmarks, max(0, landmarkCapacity))
    }

    func visit(_ destination: AtlasFolderDestination, from origin: AtlasFolderDestination? = nil) -> Move {
        let staged = stage(origin: origin)
        return Move(target: destination, revision: revision, staged: staged, proposal: staged.visit(destination))
    }

    func back(from origin: AtlasFolderDestination? = nil) -> Move? {
        let staged = stage(origin: origin)
        guard let proposal = staged.back() else { return nil }
        return Move(target: proposal.target, revision: revision, staged: staged, proposal: proposal)
    }

    func forward(from origin: AtlasFolderDestination? = nil) -> Move? {
        let staged = stage(origin: origin)
        guard let proposal = staged.forward() else { return nil }
        return Move(target: proposal.target, revision: revision, staged: staged, proposal: proposal)
    }

    func accepts(_ move: Move) -> Bool { move.revision == revision }

    /// The accepted parcel may have disappeared or changed, so its anchor may
    /// resolve to an overview. Directory and file-type scope must still match.
    @discardableResult mutating func commit(_ move: Move, resolved: AtlasFolderDestination) -> Bool {
        guard accepts(move), move.target.directory == resolved.directory,
              move.target.matchesScope(fileScope: resolved.fileScope, customExtensions: resolved.customExtensions) else {
            return false
        }
        var accepted = move.staged
        guard accepted.commit(move.proposal, resolved: resolved) else { return false }
        history = accepted
        revision = UUID()
        return true
    }

    /// A same-root refresh or accepted scope replacement retires pending work
    /// while retaining routes and session landmarks. Even a lookalike refresh
    /// invalidates proposals issued before its accepted publication.
    mutating func rebaseCurrent(_ destination: AtlasFolderDestination) {
        history.replaceCurrent(destination)
        revision = UUID()
    }

    /// Project close, root/grant replacement or discovery-policy replacement
    /// retires every route and landmark. Nothing is restored across app launches.
    mutating func reset(to destination: AtlasFolderDestination? = nil) {
        history.reset(to: destination)
        landmarks.removeAll(keepingCapacity: true)
        revision = UUID()
    }

    @discardableResult mutating func addLandmark(label: String, destination: AtlasFolderDestination) throws -> UUID {
        guard landmarks.count < landmarkCapacity else { throw LandmarkError.limit }
        let label = try Self.validatedLabel(label)
        let id = UUID()
        landmarks.append(Landmark(id: id, label: label, destination: destination))
        return id
    }

    mutating func renameLandmark(id: UUID, label: String) throws {
        let label = try Self.validatedLabel(label)
        guard let index = landmarks.firstIndex(where: { $0.id == id }) else { throw LandmarkError.missing }
        let prior = landmarks[index]
        landmarks[index] = Landmark(id: prior.id, label: label, destination: prior.destination)
    }

    @discardableResult mutating func removeLandmark(id: UUID) -> Bool {
        guard let index = landmarks.firstIndex(where: { $0.id == id }) else { return false }
        landmarks.remove(at: index)
        return true
    }

    private func stage(origin: AtlasFolderDestination?) -> AtlasNavigationHistory<AtlasFolderDestination> {
        var staged = history
        if let origin { staged.replaceCurrent(origin) }
        return staged
    }

    private static func validatedLabel(_ label: String) throws -> String {
        // Check the original byte count before trimming or retaining a copy.
        guard label.utf8.count <= maximumLabelBytes else { throw LandmarkError.invalidLabel }
        let trimmed = label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !label.unicodeScalars.contains(where: { scalar in
            switch scalar.properties.generalCategory {
            case .control, .format, .lineSeparator, .paragraphSeparator: return true
            default: return false
            }
        }) else { throw LandmarkError.invalidLabel }
        return trimmed
    }
}
