import Foundation
import CoreGraphics

@main struct AtlasCameraTests {
    static var count = 0
    static func near(_ a: Double, _ b: Double, _ message: String, tolerance: Double = 0.000001) {
        precondition(abs(a - b) <= tolerance, "\(message): \(a) != \(b)")
        count += 1
    }
    static func main() {
        let receipt = AtlasPresentedFrame()
        let revision = UUID(), replacement = UUID()
        let tap = CGPoint(x: 140, y: 260)
        precondition(receipt.worldPoint(at: tap, revision: revision) == nil,
                     "input cannot target a scene before its first accepted frame")
        receipt.record(revision: revision, scale: 2, offset: CGPoint(x: 100, y: 200))
        let visiblePoint = receipt.worldPoint(at: tap, revision: revision)!
        near(visiblePoint.x, 20, "hit uses accepted scale x")
        near(visiblePoint.y, 30, "hit uses accepted offset y")
        let unpresented = AtlasCamera()
        unpresented.scale = 8; unpresented.offsetX = 900
        precondition(unpresented.worldPoint(at: tap) != visiblePoint,
                     "pending gesture transform differs from visible hit geometry")
        precondition(receipt.worldPoint(at: tap, revision: revision) == visiblePoint,
                     "unpresented camera motion cannot change click coordinates")
        precondition(receipt.worldPoint(at: tap, revision: replacement) == nil,
                     "new source revision cannot activate old pixels")
        receipt.record(revision: replacement, scale: 4, offset: .zero)
        near(receipt.worldPoint(at: tap, revision: replacement)!.x, 35, "replacement becomes clickable after accepted frame")
        precondition(receipt.worldPoint(at: tap, revision: revision) == nil)
        receipt.record(revision: replacement, scale: .nan, offset: .zero)
        precondition(receipt.worldPoint(at: tap, revision: replacement) == nil, "invalid receipt fails closed")
        let size = CGSize(width: 800, height: 600)
        let c = AtlasCamera()
        // The pointer may start anywhere; translation never contains its origin.
        let origin = CGPoint(x: 100, y: 200)
        c.drag(location: CGPoint(x: 105, y: 207), start: origin, now: 1)
        c.drag(location: CGPoint(x: 110, y: 212), start: origin, now: 1.01)
        near(c.offsetX, 10, "cumulative non-origin drag")
        near(c.offsetY, 12, "vertical drag")
        c.tick(now: 1.02, viewport: size)
        c.tick(now: 1.04, viewport: size)
        near(c.offsetX, 10, "no inertia while dragging")
        c.endPan(now: 1.04)
        c.tick(now: 1.06, viewport: size)
        precondition(c.offsetX > 10, "release must glide")
        c.drag(translation: CGSize(width: 2, height: 0), now: 1.07)
        let interrupted = c.offsetX
        c.tick(now: 1.09, viewport: size)
        near(c.offsetX, interrupted, "new gesture interrupts old glide")
        c.endPan(now: 1.3)
        precondition(!c.isAnimating, "stationary release does not fling")

        let anchor = CGPoint(x: 173, y: 419)
        let world = c.worldPoint(at: anchor)
        c.steerZoom(by: 4, at: anchor)
        for i in 0...300 { c.tick(now: 2 + Double(i) / 120, viewport: size) }
        near(c.project(world).x, anchor.x, "zoom x anchor")
        near(c.project(world).y, anchor.y, "zoom y anchor")
        near(c.scale, c.targetScale, "zoom exact endpoint")
        precondition(!c.isAnimating, "completed camera pauses redraw")

        let beforeScroll = CGPoint(x: c.offsetX, y: c.offsetY)
        c.scroll(dx: 8, dy: -13)
        near(c.offsetX, beforeScroll.x + 8, "native scroll x")
        near(c.offsetY, beforeScroll.y - 13, "native scroll y")
        let scrollX = c.offsetX
        c.tick(now: 10, viewport: size)
        near(c.offsetX, scrollX, "native scroll does not get a second inertia")
        let originalScale = c.scale
        c.steerZoom(by: .nan, at: anchor)
        near(c.scale, originalScale, "nonfinite input ignored")
        c.reducedMotion = true
        c.steerZoom(by: 2, at: anchor)
        near(c.scale, originalScale * 2, "reduced motion direct zoom")
        precondition(!c.isAnimating, "reduced motion no flight")
        c.reducedMotion = false
        let immediateWorld = c.worldPoint(at: anchor)
        c.steerZoom(by: 1.2, at: anchor, immediate: true)
        near(c.project(immediateWorld).x, anchor.x, "pinch x anchor")
        near(c.project(immediateWorld).y, anchor.y, "pinch y anchor")
        precondition(!c.isAnimating, "native pinch needs no second animation")

        // The analytic glide must agree at 60 Hz and 120 Hz.
        func glide(_ rate: Int) -> Double {
            let camera = AtlasCamera()
            camera.drag(translation: .zero, now: 1)
            camera.drag(translation: CGSize(width: 10, height: 0), now: 1.01)
            camera.endPan(now: 1.01)
            for i in 1...rate { camera.tick(now: 1.01 + Double(i) / Double(rate), viewport: size) }
            return camera.offsetX
        }
        near(glide(60), glide(120), "refresh-independent inertia")

        let launch = AtlasCamera()
        let bounds = CGRect(x: 100, y: 200, width: 1600, height: 1200)
        precondition(!launch.fit(bounds: bounds, viewport: .zero), "zero layout must request retry")
        near(launch.scale, 0.2, "unlaid-out viewport does not change camera")
        precondition(launch.fit(bounds: bounds, viewport: size), "first usable layout fits")
        near(launch.project(CGPoint(x: bounds.midX, y: bounds.midY)).x, 400, "initial fit centers x")
        near(launch.project(CGPoint(x: bounds.midX, y: bounds.midY)).y, 300, "initial fit centers y")
        near(launch.scale, 0.48, "initial fit contains complete bounds")

        let fast = AtlasCamera()
        fast.drag(translation: .zero, now: 1)
        fast.drag(translation: CGSize(width: 150, height: 50), now: 1.0001)
        near(fast.offsetX, 150, "fast direct drag stays exact")
        near(fast.offsetY, 50, "fast direct vertical drag stays exact")
        precondition(hypot(fast.panVelocity.x, fast.panVelocity.y) <= 2400.000001,
                     "near-simultaneous input cannot produce an unbounded fling")
        fast.endPan(now: 1.0001)
        for i in 1...600 { fast.tick(now: 1.0001 + Double(i) / 60, viewport: size) }
        precondition(hypot(fast.offsetX - 150, fast.offsetY - 50) <= 400.000001,
                     "release glide stays within 400 points")
        precondition(!fast.isAnimating, "bounded fling settles")
        let wheel = AtlasCamera()
        let wheelAnchor = CGPoint(x: 271, y: 182)
        let wheelWorld = wheel.worldPoint(at: wheelAnchor)
        let initialScale = wheel.scale
        wheel.steerWheelZoom(delta: 12, precise: false, at: wheelAnchor)
        precondition(wheel.isAnimating, "plain wheel input starts eased zoom")
        near(wheel.scale, initialScale, "wheel does not jump immediately")
        for i in 0...300 { wheel.tick(now: 20 + Double(i) / 120, viewport: size) }
        precondition(wheel.scale > initialScale, "positive wheel input zooms in")
        near(wheel.project(wheelWorld).x, wheelAnchor.x, "wheel anchor x")
        near(wheel.project(wheelWorld).y, wheelAnchor.y, "wheel anchor y")
        near(AtlasCamera.wheelZoomFactor(delta: 0), 1, "zero wheel no zoom")
        near(AtlasCamera.wheelZoomFactor(delta: .nan), 1, "invalid wheel ignored")
        precondition(AtlasCamera.wheelZoomFactor(delta: -12) < 1, "negative wheel zooms out")
        precondition(AtlasCamera.wheelZoomFactor(delta: 10000) < 1.42, "single wheel event bounded")
        precondition(!wheel.isAnimating, "wheel settles into idle")
        near(AtlasCamera.wheelZoomFactor(delta: 12, precise: true), exp(0.012), "precise pixel gain")
        near(AtlasCamera.wheelZoomFactor(delta: 12), exp(0.036), "coarse line gain")
        precondition(AtlasCamera.wheelZoomFactor(delta: 10000) < 1.041, "accelerated event gains at most 4.1 percent")
        let burst = AtlasCamera()
        let burstWorld = burst.worldPoint(at: wheelAnchor), burstStart = burst.scale
        for _ in 0..<1000 { burst.steerWheelZoom(delta: 120, precise: false, at: wheelAnchor) }
        precondition(burst.targetScale / burst.scale <= exp(0.12) + 1e-12,
                     "coalesced wheel burst cannot accumulate an unbounded target")
        near(burst.scale, burstStart, "burst remains eased")
        for i in 0...300 { burst.tick(now: 30 + Double(i) / 120, viewport: size) }
        precondition(burst.scale / burstStart <= 1.128, "burst release travels at most 12.8 percent")
        near(burst.project(burstWorld).x, wheelAnchor.x, "bounded burst anchor x")
        near(burst.project(burstWorld).y, wheelAnchor.y, "bounded burst anchor y")
        for _ in 0..<1000 { burst.steerWheelZoom(delta: -120, precise: true, at: wheelAnchor) }
        precondition(burst.targetScale / burst.scale >= exp(-0.12) - 1e-12, "outward backlog is bounded too")
        burst.steerWheelZoom(delta: 1, precise: false, at: wheelAnchor)
        precondition(burst.targetScale > burst.scale, "reversal immediately discards outward backlog")
        burst.steerWheelZoom(delta: -1, precise: false, at: wheelAnchor)
        precondition(burst.targetScale < burst.scale, "reverse again discards inward backlog")
        let validTarget = burst.targetScale
        burst.steerWheelZoom(delta: .infinity, precise: true, at: wheelAnchor)
        burst.steerWheelZoom(delta: 0, precise: true, at: wheelAnchor)
        near(burst.targetScale, validTarget, "invalid and zero wheel input preserve target")
        let dense = AtlasCamera()
        dense.fillsViewport = true
        dense.fit(bounds: CGRect(x: 0, y: 0, width: 1000, height: 1000), viewport: size)
        near(dense.scale, 0.8, "dense initial view covers viewport")
        dense.steerZoom(by: 0.001, at: CGPoint(x: 400, y: 300), immediate: true)
        near(dense.scale, 0.8, "cannot zoom out into empty outer space")
        dense.drag(translation: CGSize(width: 10000, height: -10000), now: 100)
        precondition(dense.offsetX <= 0 && dense.offsetX + 1000 * dense.scale >= 800,
                     "dense pan keeps horizontal content coverage")
        precondition(dense.offsetY <= 0 && dense.offsetY + 1000 * dense.scale >= 600,
                     "dense pan keeps vertical content coverage")
        dense.endPan(now: 100)
        dense.updateViewport(CGSize(width: 1200, height: 700))
        precondition(dense.scale >= 1.2, "resize retains filled viewport")
        dense.fit(bounds: CGRect(x: 0, y: 0, width: 1000, height: 1000), viewport: size)
        dense.steerZoom(by: 2, at: CGPoint(x: 400, y: 300), immediate: true)
        dense.steerZoom(by: 0.5, at: CGPoint(x: 400, y: 300))
        dense.updateViewport(CGSize(width: 1200, height: 700))
        for i in 0...300 { dense.tick(now: 110 + Double(i) / 120, viewport: size) }
        precondition(dense.scale >= 1.2, "resize clamps an in-flight zoom-out destination")
        let project = CGRect(x: 0, y: 0, width: 10000, height: 10000)
        let destination = CGRect(x: 4200, y: 3100, width: 200, height: 400)
        func flightCamera() -> AtlasCamera {
            let result = AtlasCamera(); result.fit(bounds: project, viewport: size); return result
        }
        func settle(_ camera: AtlasCamera, start: Double = 200) {
            for i in 0...900 { camera.tick(now: start + Double(i) / 120, viewport: size) }
            precondition(!camera.isAnimating, "focus must converge and pause redraw")
        }
        let flight = flightCamera()
        let startScale = flight.scale, startX = flight.offsetX
        precondition(flight.focus(bounds: destination, viewport: size))
        precondition(flight.contentBounds == project, "focus preserves project pan bounds")
        near(flight.scale, startScale, "focus does not teleport scale")
        near(flight.offsetX, startX, "focus does not teleport offset")
        precondition(flight.isAnimating)
        settle(flight)
        near(flight.scale, 1.32, "focus padded target scale")
        near(flight.project(CGPoint(x: destination.midX, y: destination.midY)).x, 400, "focus center x")
        near(flight.project(CGPoint(x: destination.midX, y: destination.midY)).y, 300, "focus center y")
        let reduced = flightCamera(); reduced.reducedMotion = true
        precondition(reduced.focus(bounds: destination, viewport: size))
        near(reduced.scale, flight.scale, "reduced motion same endpoint")
        near(reduced.offsetX, flight.offsetX, "reduced motion same center")
        precondition(!reduced.isAnimating && reduced.contentBounds == project)
        for mode in 0..<3 {
            let interrupted = flightCamera(); interrupted.focus(bounds: destination, viewport: size)
            interrupted.tick(now: 1, viewport: size); interrupted.tick(now: 1.05, viewport: size)
            let before = CGPoint(x: interrupted.offsetX, y: interrupted.offsetY)
            if mode == 0 {
                let anchor = CGPoint(x: 237, y: 139), world = interrupted.worldPoint(at: CGPoint(x: 237, y: 139))
                interrupted.steerWheelZoom(delta: 12, precise: false, at: anchor)
                settle(interrupted)
                near(interrupted.project(world).x, anchor.x, "wheel cancels focus and anchors x")
                near(interrupted.project(world).y, anchor.y, "wheel cancels focus and anchors y")
            } else {
                if mode == 1 { interrupted.drag(translation: CGSize(width: 7, height: 9), now: 2); interrupted.endPan(now: 2.3) }
                else { interrupted.scroll(dx: 7, dy: 9) }
                settle(interrupted)
                near(interrupted.offsetX, before.x + 7, "pan cancels flight x")
                near(interrupted.offsetY, before.y + 9, "pan cancels flight y")
            }
            precondition(interrupted.contentBounds == project)
        }
        let invalid = flightCamera()
        let invalidScale = invalid.scale, invalidX = invalid.offsetX
        for box in [CGRect.zero, CGRect(x: Double.nan, y: 0, width: 1, height: 1), CGRect(x: 0, y: 0, width: Double.infinity, height: 1)] {
            precondition(!invalid.focus(bounds: box, viewport: size))
        }
        precondition(!invalid.focus(bounds: destination, viewport: .zero))
        near(invalid.scale, invalidScale, "invalid focus leaves scale")
        near(invalid.offsetX, invalidX, "invalid focus leaves position")
        precondition(!invalid.isAnimating && invalid.contentBounds == project)
        for edge in [CGRect(x: 0, y: 0, width: 100, height: 100), CGRect(x: 9900, y: 9900, width: 100, height: 100)] {
            let bounded = AtlasCamera(); bounded.fillsViewport = true
            bounded.fit(bounds: project, viewport: size)
            precondition(bounded.focus(bounds: edge, viewport: size)); settle(bounded)
            precondition(bounded.contentBounds == project)
            precondition(bounded.offsetX <= 0 && bounded.offsetY <= 0)
            precondition(bounded.offsetX + project.width * bounded.scale >= size.width && bounded.offsetY + project.height * bounded.scale >= size.height)
        }
        let resizedFlight = AtlasCamera(); resizedFlight.fillsViewport = true
        resizedFlight.fit(bounds: project, viewport: size)
        resizedFlight.focus(bounds: CGRect(x: 0, y: 0, width: 100, height: 100), viewport: size)
        resizedFlight.tick(now: 1, viewport: size); resizedFlight.tick(now: 1.05, viewport: size)
        let enlarged = CGSize(width: 1400, height: 1000)
        resizedFlight.updateViewport(enlarged)
        for i in 0...900 { resizedFlight.tick(now: 2 + Double(i) / 120, viewport: enlarged) }
        precondition(!resizedFlight.isAnimating, "edge focus must settle after viewport resize")
        precondition(resizedFlight.offsetX + project.width * resizedFlight.scale >= enlarged.width &&
                     resizedFlight.offsetY + project.height * resizedFlight.scale >= enlarged.height)
        precondition(resizedFlight.contentBounds == project)
        print("AtlasCamera: \(count) numeric assertions plus lifecycle checks passed")
    }
}
