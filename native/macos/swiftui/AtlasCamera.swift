import Foundation
import CoreGraphics
import Observation

/// A camera location relative to one accepted file parcel. The owner keeps the
/// original root-relative file path separately and resolves new parcel geometry
/// before restoring. Zoom is relative to fitting that parcel in the viewport;
/// no obsolete layout coordinates or screen dimensions survive a folder change.
struct AtlasParcelCameraAnchor: Equatable, Sendable {
    let relativeX: Double
    let relativeY: Double
    let zoomFactor: Double

    init?(relativeX: Double, relativeY: Double, zoomFactor: Double) {
        // The nearest parcel can be outside the viewport center, so relative
        // coordinates are not restricted to its interior. Bound the ratios to
        // keep even a corrupt or degenerate parcel from retaining huge values.
        guard relativeX.isFinite, relativeY.isFinite, zoomFactor.isFinite,
              abs(relativeX) <= 1_000_000, abs(relativeY) <= 1_000_000,
              (0.000001...1_000_000).contains(zoomFactor) else { return nil }
        self.relativeX = relativeX; self.relativeY = relativeY; self.zoomFactor = zoomFactor
    }
}

/// Main-thread presentation receipt, deliberately not observable: accepting a
/// frame must not schedule another SwiftUI render. Input targets visible pixels.
final class AtlasPresentedFrame {
    private var revision: UUID?
    private var contentRevision: UUID?
    private var scale = 1.0
    private var offset = CGPoint.zero

    func record(revision: UUID, scale: Double, offset: CGPoint, contentRevision: UUID? = nil) {
        guard scale.isFinite, scale > 0, offset.x.isFinite, offset.y.isFinite else {
            self.revision = nil
            return
        }
        self.revision = revision; self.contentRevision = contentRevision
        self.scale = scale; self.offset = offset
    }

    /// Use the displayed pose for the limit test and hit, even when the camera
    /// has already requested a different scale or layout for its next frame.
    func zoomLimitPoint(at point: CGPoint, by factor: Double, revision: UUID, contentRevision: UUID) -> CGPoint? {
        guard factor.isFinite, factor > 1, scale >= AtlasCamera.maximumScale * (1 - 0.000001) else { return nil }
        return worldPoint(at: point, revision: revision, contentRevision: contentRevision)
    }

    func worldPoint(at point: CGPoint, revision: UUID, contentRevision: UUID? = nil) -> CGPoint? {
        guard self.revision == revision, self.contentRevision == contentRevision,
              point.x.isFinite, point.y.isFinite else { return nil }
        let world = CGPoint(x: (point.x - offset.x) / scale, y: (point.y - offset.y) / scale)
        return world.x.isFinite && world.y.isFinite ? world : nil
    }
}

/// Screen-space interaction state. Native momentum is applied directly; only
/// pointer drags generate our own glide. No I/O or content work belongs here.
@Observable final class AtlasCamera {
    static let maximumScale = 4096.0

    /// Repack a file if even its shorter side at the camera ceiling would
    /// span less than one 16-point text row.
    static func needsFileFocus(_ bounds: CGRect) -> Bool {
        guard !bounds.isNull, !bounds.isInfinite, bounds.size.width.isFinite, bounds.size.height.isFinite,
              bounds.size.width > 0, bounds.size.height > 0 else { return false }
        return min(bounds.size.width, bounds.size.height) < 16 / maximumScale
    }

    /// World centers survive viewport resizing; pixel offsets do not. These
    /// positions belong only to the bound atlas layout, never to source bytes.
    private struct Location: Equatable {
        let center: CGPoint
        let scale: Double
    }
    private var navigationRevision: UUID?
    private var navigationHistory = AtlasNavigationHistory<Location>()
    private enum PresentationDestination {
        case parcel(AtlasParcelCameraAnchor, CGRect)
        case overview(CGRect)
    }
    private struct PresentationRestore {
        let revision: UUID
        let destination: PresentationDestination
    }
    private var pendingPresentationRestore: PresentationRestore?
    private var presentationRevision: UUID?
    var scale: Double = 0.2
    var offsetX: Double = 0
    var offsetY: Double = 0
    private(set) var targetScale: Double = 0.2
    private(set) var panVelocity = CGPoint.zero
    private(set) var isDragging = false
    var reducedMotion = false { didSet { if reducedMotion { stopMotion() } } }
    var contentBounds = CGRect(x: 0, y: 0, width: 4096, height: 4096)
    var fillsViewport = false
    private var viewportSize = CGSize.zero
    private var anchorScreen = CGPoint.zero
    private var anchorWorld = CGPoint.zero
    private var anchorActive = false
    private var lastTick: Double?
    private var lastTranslation = CGSize.zero
    private var lastDragTime: Double?
    private var focusCenter: CGPoint?

    var isAnimating: Bool {
        focusCenter != nil || anchorActive || (!isDragging && hypot(panVelocity.x, panVelocity.y) > 1)
    }

    var retainedNavigationCount: Int { navigationHistory.retainedCount }
    func acceptsNavigation(revision: UUID) -> Bool { navigationRevision == revision }
    func canGoBack(revision: UUID) -> Bool {
        navigationRevision == revision && navigationHistory.canBack
    }
    func canGoForward(revision: UUID) -> Bool {
        navigationRevision == revision && navigationHistory.canForward
    }

    /// Reflow or project/scope replacement retires the coordinate domain. An
    /// old view's queued button action cannot navigate the replacement atlas.
    func bindNavigation(to revision: UUID, preservingHistory: Bool = false) {
        guard navigationRevision != revision else { return }
        let preservesPresentation = preservingHistory && navigationRevision != nil && presentationRevision == navigationRevision
        stopMotion()
        pendingPresentationRestore = nil
        isDragging = false
        lastTranslation = .zero
        lastDragTime = nil
        navigationRevision = revision
        presentationRevision = preservesPresentation ? revision : nil
        if !preservingHistory { navigationHistory.reset() }
    }

    /// Use the same intended endpoint as an anchor snapshot when choosing its
    /// semantic parcel. An unpresented replacement layout has no old pose to save.
    func presentationCenter(revision: UUID) -> CGPoint? {
        guard navigationRevision == revision else { return nil }
        if let pending = pendingPresentationRestore, pending.revision == revision {
            switch pending.destination {
            case let .parcel(anchor, bounds): return Self.anchorCenter(anchor, bounds: bounds)
            case let .overview(bounds): return CGPoint(x: bounds.midX, y: bounds.midY)
            }
        }
        guard presentationRevision == revision else { return nil }
        return navigationLocation(viewport: viewportSize)?.center
    }

    /// Only a not-yet-consumed restore may keep its original semantic parcel.
    /// Ordinary panning snapshots should choose the parcel now under the center.
    func pendingPresentationAnchor(relativeTo bounds: CGRect, revision: UUID) -> AtlasParcelCameraAnchor? {
        guard navigationRevision == revision, let pending = pendingPresentationRestore, pending.revision == revision,
              case let .parcel(anchor, restoredBounds) = pending.destination, restoredBounds == bounds else { return nil }
        return anchor
    }

    func presentationAnchor(relativeTo bounds: CGRect, revision: UUID) -> AtlasParcelCameraAnchor? {
        guard navigationRevision == revision, Self.validPresentationBounds(bounds) else { return nil }
        if let anchor = pendingPresentationAnchor(relativeTo: bounds, revision: revision) { return anchor }
        guard presentationRevision == revision,
              let location = navigationLocation(viewport: viewportSize) else { return nil }
        let fitScale = min(viewportSize.width / bounds.width, viewportSize.height / bounds.height)
        guard fitScale.isFinite, fitScale > 0 else { return nil }
        return AtlasParcelCameraAnchor(relativeX: (location.center.x - bounds.minX) / bounds.width,
            relativeY: (location.center.y - bounds.minY) / bounds.height,
            zoomFactor: location.scale / fitScale)
    }

    /// Restore immediately when possible and retain one geometry-qualified
    /// intent for the replacement view's first usable viewport. Hidden atlases
    /// can stage this without I/O or borrowing another layout's coordinates.
    @discardableResult
    func restorePresentationAnchor(_ anchor: AtlasParcelCameraAnchor, relativeTo bounds: CGRect,
        revision: UUID) -> Bool {
        guard navigationRevision == revision, Self.anchorCenter(anchor, bounds: bounds) != nil else { return false }
        return stagePresentationRestore(PresentationRestore(revision: revision, destination: .parcel(anchor, bounds)))
    }

    @discardableResult
    func restorePresentationFit(revision: UUID) -> Bool {
        guard navigationRevision == revision, Self.validPresentationBounds(contentBounds) else { return false }
        return stagePresentationRestore(PresentationRestore(revision: revision, destination: .overview(contentBounds)))
    }

    private func stagePresentationRestore(_ restore: PresentationRestore) -> Bool {
        if Self.validViewport(viewportSize) {
            guard applyPresentationRestore(restore, viewport: viewportSize) else { return false }
        } else {
            stopMotion()
            isDragging = false
            lastTranslation = .zero
            lastDragTime = nil
        }
        pendingPresentationRestore = restore
        return true
    }

    private func applyPresentationRestore(_ restore: PresentationRestore, viewport: CGSize) -> Bool {
        guard navigationRevision == restore.revision, Self.validViewport(viewport) else { return false }
        switch restore.destination {
        case let .overview(bounds):
            return fit(bounds: bounds, viewport: viewport)
        case let .parcel(anchor, bounds):
            guard var center = Self.anchorCenter(anchor, bounds: bounds) else { return false }
            let fitScale = min(viewport.width / bounds.width, viewport.height / bounds.height)
            let requestedScale = fitScale * anchor.zoomFactor
            guard fitScale.isFinite, fitScale > 0, requestedScale.isFinite, requestedScale > 0 else { return false }
            let restoredScale = max(minimumScale(viewport: viewport), min(Self.maximumScale, requestedScale))
            if fillsViewport {
                guard Self.validPresentationBounds(contentBounds) else { return false }
                let halfWidth = viewport.width / (2 * restoredScale), halfHeight = viewport.height / (2 * restoredScale)
                center = CGPoint(
                    x: min(contentBounds.maxX - halfWidth, max(contentBounds.minX + halfWidth, center.x)),
                    y: min(contentBounds.maxY - halfHeight, max(contentBounds.minY + halfHeight, center.y)))
            }
            let x = viewport.width / 2 - center.x * restoredScale
            let y = viewport.height / 2 - center.y * restoredScale
            guard restoredScale.isFinite, center.x.isFinite, center.y.isFinite, x.isFinite, y.isFinite else { return false }
            stopMotion()
            isDragging = false
            lastTranslation = .zero
            lastDragTime = nil
            viewportSize = viewport
            scale = restoredScale; targetScale = restoredScale
            offsetX = x; offsetY = y
            presentationRevision = restore.revision
            constrainToContent()
            return true
        }
    }

    private static func validViewport(_ viewport: CGSize) -> Bool {
        viewport.width.isFinite && viewport.height.isFinite && viewport.width > 1 && viewport.height > 1
    }

    private static func validPresentationBounds(_ bounds: CGRect) -> Bool {
        bounds.minX.isFinite && bounds.minY.isFinite && bounds.width.isFinite && bounds.height.isFinite
            && bounds.maxX.isFinite && bounds.maxY.isFinite && bounds.midX.isFinite && bounds.midY.isFinite
            && bounds.width > 0 && bounds.height > 0
    }

    private static func anchorCenter(_ anchor: AtlasParcelCameraAnchor, bounds: CGRect) -> CGPoint? {
        guard validPresentationBounds(bounds) else { return nil }
        let center = CGPoint(x: bounds.minX + anchor.relativeX * bounds.width,
            y: bounds.minY + anchor.relativeY * bounds.height)
        return center.x.isFinite && center.y.isFinite ? center : nil
    }

    /// Refresh preserves the coordinate frame. Show added files can focus the
    /// new neighborhood using the last usable viewport.
    @discardableResult
    func navigateFocus(bounds: CGRect, revision: UUID) -> Bool {
        navigateFocus(bounds: bounds, viewport: viewportSize, revision: revision)
    }

    @discardableResult
    func navigateFocus(bounds: CGRect, viewport: CGSize, revision: UUID) -> Bool {
        guard navigationRevision == revision, let origin = navigationLocation(viewport: viewport),
              focus(bounds: bounds, viewport: viewport),
              let target = navigationLocation(viewport: viewport) else { return false }
        recordNavigation(from: origin, to: target)
        return true
    }

    @discardableResult
    func navigateFit(bounds: CGRect, viewport: CGSize, revision: UUID) -> Bool {
        guard navigationRevision == revision, let origin = navigationLocation(viewport: viewport),
              fit(bounds: bounds, viewport: viewport),
              let target = navigationLocation(viewport: viewport) else { return false }
        recordNavigation(from: origin, to: target)
        return true
    }

    @discardableResult
    func goBack(viewport: CGSize, revision: UUID) -> Bool {
        navigateHistory(back: true, viewport: viewport, revision: revision)
    }
    @discardableResult
    func goForward(viewport: CGSize, revision: UUID) -> Bool {
        navigateHistory(back: false, viewport: viewport, revision: revision)
    }

    private func navigationLocation(viewport: CGSize) -> Location? {
        guard viewport.width.isFinite, viewport.height.isFinite, viewport.width > 1, viewport.height > 1 else { return nil }
        // A second explicit action during a flight remembers the intended
        // endpoint. Direct input cancels focusCenter and resumes from the view
        // actually reached, rather than adding each animation/gesture frame.
        let center = focusCenter ?? worldPoint(at: CGPoint(x: viewport.width / 2, y: viewport.height / 2))
        let value = focusCenter == nil ? scale : targetScale
        guard center.x.isFinite, center.y.isFinite, value.isFinite, value > 0 else { return nil }
        return Location(center: center, scale: value)
    }

    private func recordNavigation(from origin: Location, to target: Location) {
        // Floating-point inversion at a settled endpoint can differ by a
        // fraction of a pixel. Repeating the same focus must not add a step.
        if abs(log(origin.scale / target.scale)) < 1e-10,
           hypot(origin.center.x - target.center.x, origin.center.y - target.center.y) * target.scale < 1e-6 {
            navigationHistory.replaceCurrent(target)
            return
        }
        navigationHistory.replaceCurrent(origin)
        let move = navigationHistory.visit(target)
        navigationHistory.commit(move, resolved: target)
    }

    private func navigateHistory(back: Bool, viewport: CGSize, revision: UUID) -> Bool {
        guard navigationRevision == revision,
              let origin = navigationLocation(viewport: viewport),
              let proposal = back ? navigationHistory.back() : navigationHistory.forward() else { return false }
        var destination = proposal.target
        if fillsViewport {
            guard contentBounds.width > 0, contentBounds.height > 0 else { return false }
            let scale = max(destination.scale, max(viewport.width / contentBounds.width, viewport.height / contentBounds.height))
            guard scale.isFinite, scale > 0 else { return false }
            let halfWidth = viewport.width / (2 * scale), halfHeight = viewport.height / (2 * scale)
            destination = Location(center: CGPoint(
                x: min(contentBounds.maxX - halfWidth, max(contentBounds.minX + halfWidth, destination.center.x)),
                y: min(contentBounds.maxY - halfHeight, max(contentBounds.minY + halfHeight, destination.center.y))), scale: scale)
        }
        guard destination.center.x.isFinite, destination.center.y.isFinite,
              (viewport.width / 2 - destination.center.x * destination.scale).isFinite,
              (viewport.height / 2 - destination.center.y * destination.scale).isFinite else { return false }
        // Refreshing current preserves the latest manually explored view for
        // the return journey without creating a history entry for every pan.
        navigationHistory.replaceCurrent(origin)
        guard let move = back ? navigationHistory.back() : navigationHistory.forward(),
              navigationHistory.commit(move, resolved: destination) else { return false }
        pendingPresentationRestore = nil
        stopMotion()
        viewportSize = viewport
        targetScale = destination.scale
        focusCenter = destination.center
        presentationRevision = revision
        if reducedMotion {
            scale = targetScale
            offsetX = viewport.width / 2 - destination.center.x * scale
            offsetY = viewport.height / 2 - destination.center.y * scale
            focusCenter = nil
            constrainToContent()
        }
        return true
    }

    func stopMotion() {
        targetScale = scale
        focusCenter = nil
        anchorActive = false
        panVelocity = .zero
        lastTick = nil
    }

    @discardableResult
    func fit(bounds: CGRect, viewport: CGSize) -> Bool {
        guard bounds.minX.isFinite, bounds.minY.isFinite,
              bounds.width.isFinite, bounds.height.isFinite,
              bounds.maxX.isFinite, bounds.maxY.isFinite, bounds.midX.isFinite, bounds.midY.isFinite,
              viewport.width.isFinite, viewport.height.isFinite,
              bounds.width > 0, bounds.height > 0, viewport.width > 1, viewport.height > 1 else { return false }
        let horizontal = viewport.width / bounds.width
        let vertical = viewport.height / bounds.height
        let fittedScale: Double = max(0.000001, fillsViewport ? max(horizontal, vertical) : min(horizontal, vertical) * 0.96)
        let x = viewport.width / 2 - bounds.midX * fittedScale
        let y = viewport.height / 2 - bounds.midY * fittedScale
        guard fittedScale.isFinite, x.isFinite, y.isFinite else { return false }
        pendingPresentationRestore = nil
        stopMotion()
        contentBounds = bounds
        viewportSize = viewport
        scale = fittedScale
        targetScale = scale
        offsetX = x
        offsetY = y
        presentationRevision = navigationRevision
        return true
    }

    /// Fly to captured source geometry without loading, parsing, or reshaping it.
    /// A logarithmic scale approach keeps large overview-to-source jumps smooth.
    @discardableResult
    func focus(bounds: CGRect, viewport: CGSize) -> Bool {
        guard bounds.minX.isFinite, bounds.minY.isFinite,
              bounds.width.isFinite, bounds.height.isFinite,
              bounds.maxX.isFinite, bounds.maxY.isFinite, bounds.midX.isFinite, bounds.midY.isFinite,
              bounds.width > 0, bounds.height > 0,
              viewport.width.isFinite, viewport.height.isFinite,
              viewport.width > 1, viewport.height > 1 else { return false }
        let fittedScale = max(minimumScale(viewport: viewport), min(Self.maximumScale,
            min(viewport.width / bounds.width, viewport.height / bounds.height) * 0.88))
        var destination = CGPoint(x: bounds.midX, y: bounds.midY)
        if fillsViewport {
            let halfWidth = viewport.width / (2 * fittedScale)
            let halfHeight = viewport.height / (2 * fittedScale)
            destination.x = min(contentBounds.maxX - halfWidth, max(contentBounds.minX + halfWidth, destination.x))
            destination.y = min(contentBounds.maxY - halfHeight, max(contentBounds.minY + halfHeight, destination.y))
        }
        guard fittedScale.isFinite, destination.x.isFinite, destination.y.isFinite,
              (viewport.width / 2 - destination.x * fittedScale).isFinite,
              (viewport.height / 2 - destination.y * fittedScale).isFinite else { return false }
        pendingPresentationRestore = nil
        stopMotion()
        viewportSize = viewport
        targetScale = fittedScale
        focusCenter = destination
        presentationRevision = navigationRevision
        if reducedMotion {
            scale = targetScale
            offsetX = viewport.width / 2 - destination.x * scale
            offsetY = viewport.height / 2 - destination.y * scale
            focusCenter = nil
            constrainToContent()
        }
        return true
    }

    private var minimumScale: Double {
        minimumScale(viewport: viewportSize)
    }

    private func minimumScale(viewport: CGSize) -> Double {
        guard fillsViewport, contentBounds.width > 0, contentBounds.height > 0 else { return 0.000001 }
        return max(0.000001, max(viewport.width / contentBounds.width, viewport.height / contentBounds.height))
    }

    func updateViewport(_ size: CGSize) {
        guard size.width > 1, size.height > 1, size.width.isFinite, size.height.isFinite else { return }
        viewportSize = size
        if let restore = pendingPresentationRestore {
            pendingPresentationRestore = nil
            _ = applyPresentationRestore(restore, viewport: size)
        }
        guard fillsViewport else { return }
        targetScale = max(targetScale, minimumScale)
        if scale < minimumScale {
            stopMotion()
            scale = minimumScale
            targetScale = scale
        }
        if let destination = focusCenter {
            let halfWidth = size.width / (2 * targetScale)
            let halfHeight = size.height / (2 * targetScale)
            focusCenter = CGPoint(
                x: min(contentBounds.maxX - halfWidth, max(contentBounds.minX + halfWidth, destination.x)),
                y: min(contentBounds.maxY - halfHeight, max(contentBounds.minY + halfHeight, destination.y)))
        }
        constrainToContent()
    }

    private func constrainToContent() {
        guard fillsViewport, viewportSize.width > 1, viewportSize.height > 1 else { return }
        let x = min(-contentBounds.minX * scale, max(viewportSize.width - contentBounds.maxX * scale, offsetX))
        let y = min(-contentBounds.minY * scale, max(viewportSize.height - contentBounds.maxY * scale, offsetY))
        if x != offsetX { panVelocity.x = 0 }
        if y != offsetY { panVelocity.y = 0 }
        offsetX = x
        offsetY = y
    }

    func worldPoint(at point: CGPoint) -> CGPoint {
        CGPoint(x: (point.x - offsetX) / scale, y: (point.y - offsetY) / scale)
    }

    func project(_ point: CGPoint) -> CGPoint {
        CGPoint(x: point.x * scale + offsetX, y: point.y * scale + offsetY)
    }

    /// Precise devices report pixel deltas; a coarse wheel reports line deltas.
    /// Keep both gentle, including coalesced and accelerated wheel events.
    static func wheelZoomFactor(delta: Double, precise: Bool = false) -> Double {
        guard delta.isFinite else { return 1 }
        return exp(max(-0.04, min(0.04, delta * (precise ? 0.001 : 0.003))))
    }

    func steerWheelZoom(delta: Double, precise: Bool, at point: CGPoint) {
        guard delta.isFinite, delta != 0, point.x.isFinite, point.y.isFinite else { return }
        if focusCenter != nil { stopMotion() }
        let factor = Self.wheelZoomFactor(delta: delta, precise: precise)
        // A burst must not bank seconds of zoom ahead of the displayed image.
        // Reversing direction cancels that backlog instead of fighting it.
        if (factor > 1 && targetScale < scale) || (factor < 1 && targetScale > scale) {
            targetScale = scale
        }
        let destination = min(scale * exp(0.12), max(scale * exp(-0.12), targetScale * factor))
        steerZoom(by: destination / targetScale, at: point)
    }

    func steerZoom(by factor: Double, at point: CGPoint, immediate: Bool = false) {
        guard factor.isFinite, factor > 0, point.x.isFinite, point.y.isFinite else { return }
        pendingPresentationRestore = nil
        if focusCenter != nil { stopMotion() }
        panVelocity = .zero
        anchorScreen = point
        anchorWorld = worldPoint(at: point)
        targetScale = max(minimumScale, min(Self.maximumScale, targetScale * factor))
        if immediate || reducedMotion {
            scale = targetScale
            offsetX = point.x - anchorWorld.x * scale
            offsetY = point.y - anchorWorld.y * scale
            anchorActive = false
            constrainToContent()
        } else {
            if !anchorActive { lastTick = nil }
            anchorActive = true
        }
    }

    /// Native adapter: both locations are in the same view coordinate space.
    func drag(location: CGPoint, start: CGPoint, now: Double) {
        drag(translation: CGSize(width: location.x - start.x, height: location.y - start.y), now: now)
    }

    /// translation is cumulative from the gesture origin, never a location.
    func drag(translation: CGSize, now: Double) {
        guard translation.width.isFinite, translation.height.isFinite, now.isFinite else { return }
        pendingPresentationRestore = nil
        // A keyboard history action can begin while a mouse button remains
        // down. Its next delta resumes that drag and cancels the flight.
        if focusCenter != nil || anchorActive { stopMotion() }
        if !isDragging {
            stopMotion()
            isDragging = true
            lastTranslation = .zero
            lastDragTime = nil
        }
        let dx = translation.width - lastTranslation.width
        let dy = translation.height - lastTranslation.height
        offsetX += dx
        offsetY += dy
        if let previous = lastDragTime, now > previous, now - previous <= 0.1 {
            let dt = max(1.0 / 120, now - previous)
            let vx = dx / dt
            let vy = dy / dt
            // Coalesced or synthetic events can arrive almost together. Keep
            // direct movement exact, but bound the release glide to 400 points
            // (2400 points/s divided by the decay constant of 6).
            let limit = min(1, 2400 / max(1, hypot(vx, vy)))
            panVelocity = CGPoint(x: vx * limit, y: vy * limit)
        } else {
            panVelocity = .zero
        }
        lastTranslation = translation
        lastDragTime = now
        constrainToContent()
    }

    func endPan(now: Double) {
        isDragging = false
        if reducedMotion || lastDragTime.map({ now - $0 > 0.1 }) != false { panVelocity = .zero }
        lastTranslation = .zero
        lastDragTime = nil
        lastTick = now
    }

    /// AppKit already supplies decaying momentum-phase deltas. Never add glide.
    func scroll(dx: Double, dy: Double) {
        guard dx.isFinite, dy.isFinite else { return }
        pendingPresentationRestore = nil
        stopMotion()
        offsetX += dx
        offsetY += dy
        constrainToContent()
    }

    func tick(now: Double, viewport: CGSize) {
        guard now.isFinite else { return }
        let previous = lastTick ?? now
        lastTick = now
        let dt = min(0.05, max(0, now - previous))
        if let destination = focusCenter {
            let remaining = exp(-dt * 10)
            let center = worldPoint(at: CGPoint(x: viewportSize.width / 2, y: viewportSize.height / 2))
            scale = exp(log(targetScale) + (log(scale) - log(targetScale)) * remaining)
            let x = destination.x + (center.x - destination.x) * remaining
            let y = destination.y + (center.y - destination.y) * remaining
            offsetX = viewportSize.width / 2 - x * scale
            offsetY = viewportSize.height / 2 - y * scale
            if abs(log(scale / targetScale)) < 0.0001 && hypot(x - destination.x, y - destination.y) * scale < 0.1 {
                scale = targetScale
                offsetX = viewportSize.width / 2 - destination.x * scale
                offsetY = viewportSize.height / 2 - destination.y * scale
                focusCenter = nil
            }
        }
        if anchorActive {
            // Exponential approach is independent of the refresh interval.
            let remaining = exp(-dt * 16)
            scale = targetScale + (scale - targetScale) * remaining
            if abs(targetScale - scale) <= targetScale * 0.0001 {
                scale = targetScale
                anchorActive = false
            }
            offsetX = anchorScreen.x - anchorWorld.x * scale
            offsetY = anchorScreen.y - anchorWorld.y * scale
        }
        if !isDragging && hypot(panVelocity.x, panVelocity.y) > 1 {
            let decay = exp(-dt * 6)
            // Integrate exponential velocity analytically, not v * dt.
            offsetX += Double(panVelocity.x) * (1 - decay) / 6
            offsetY += Double(panVelocity.y) * (1 - decay) / 6
            panVelocity.x *= decay
            panVelocity.y *= decay
            if hypot(panVelocity.x, panVelocity.y) <= 1 { panVelocity = .zero }
        }
        // Dense text mode keeps the finite text field covering the viewport.
        // Interior zoom remains pointer-anchored; only outer edges constrain it.
        constrainToContent()
    }
}
