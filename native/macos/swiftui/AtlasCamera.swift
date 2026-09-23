import Foundation
import CoreGraphics
import Observation

/// Main-thread presentation receipt, deliberately not observable: accepting a
/// frame must not schedule another SwiftUI render. Input targets visible pixels.
final class AtlasPresentedFrame {
    private var revision: UUID?
    private var scale = 1.0
    private var offset = CGPoint.zero

    func record(revision: UUID, scale: Double, offset: CGPoint) {
        guard scale.isFinite, scale > 0, offset.x.isFinite, offset.y.isFinite else {
            self.revision = nil
            return
        }
        self.revision = revision; self.scale = scale; self.offset = offset
    }

    func worldPoint(at point: CGPoint, revision: UUID) -> CGPoint? {
        guard self.revision == revision, point.x.isFinite, point.y.isFinite else { return nil }
        let world = CGPoint(x: (point.x - offset.x) / scale, y: (point.y - offset.y) / scale)
        return world.x.isFinite && world.y.isFinite ? world : nil
    }
}

/// Screen-space interaction state. Native momentum is applied directly; only
/// pointer drags generate our own glide. No I/O or content work belongs here.
@Observable final class AtlasCamera {
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
              viewport.width.isFinite, viewport.height.isFinite,
              bounds.width > 0, bounds.height > 0, viewport.width > 1, viewport.height > 1 else { return false }
        stopMotion()
        contentBounds = bounds
        viewportSize = viewport
        let horizontal = viewport.width / bounds.width
        let vertical = viewport.height / bounds.height
        scale = max(0.000001, fillsViewport ? max(horizontal, vertical) : min(horizontal, vertical) * 0.96)
        targetScale = scale
        offsetX = viewport.width / 2 - bounds.midX * scale
        offsetY = viewport.height / 2 - bounds.midY * scale
        return true
    }

    /// Fly to captured source geometry without loading, parsing, or reshaping it.
    /// A logarithmic scale approach keeps large overview-to-source jumps smooth.
    @discardableResult
    func focus(bounds: CGRect, viewport: CGSize) -> Bool {
        guard bounds.minX.isFinite, bounds.minY.isFinite,
              bounds.width.isFinite, bounds.height.isFinite,
              bounds.width > 0, bounds.height > 0,
              viewport.width.isFinite, viewport.height.isFinite,
              viewport.width > 1, viewport.height > 1 else { return false }
        stopMotion()
        viewportSize = viewport
        targetScale = max(minimumScale, min(4096,
            min(viewport.width / bounds.width, viewport.height / bounds.height) * 0.88))
        var destination = CGPoint(x: bounds.midX, y: bounds.midY)
        if fillsViewport {
            let halfWidth = viewport.width / (2 * targetScale)
            let halfHeight = viewport.height / (2 * targetScale)
            destination.x = min(contentBounds.maxX - halfWidth, max(contentBounds.minX + halfWidth, destination.x))
            destination.y = min(contentBounds.maxY - halfHeight, max(contentBounds.minY + halfHeight, destination.y))
        }
        focusCenter = destination
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
        guard fillsViewport, contentBounds.width > 0, contentBounds.height > 0 else { return 0.000001 }
        return max(0.000001, max(viewportSize.width / contentBounds.width, viewportSize.height / contentBounds.height))
    }

    func updateViewport(_ size: CGSize) {
        guard size.width > 1, size.height > 1, size.width.isFinite, size.height.isFinite else { return }
        viewportSize = size
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
        if focusCenter != nil { stopMotion() }
        panVelocity = .zero
        anchorScreen = point
        anchorWorld = worldPoint(at: point)
        targetScale = max(minimumScale, min(4096, targetScale * factor))
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
