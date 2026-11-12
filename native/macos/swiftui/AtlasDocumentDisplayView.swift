import SwiftUI
import AppKit
import CoreText

/// Native drawing of the supplied glyph IDs. The engine owns all wrapping,
/// shaping, layout and source correspondence. Text mode remains the native text
/// selection route; this surface selects qualified enclosing source blocks.
@MainActor struct AtlasDocumentDisplayView: View {
    let page: AtlasDocumentDisplayPage
    let fonts: [AtlasDocumentDisplayFont]
    let enabled: Bool
    let openSource: (AtlasDocumentDisplayItem) -> Void
    @AtlasAppearance private var appearance

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Click an element to show its captured Markdown block.")
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                Menu("Show source") {
                    ForEach(page.items.filter { $0.original != nil && $0.readingText != nil }) { item in
                        Button(String((item.readingText ?? "").prefix(90))) { openSource(item) }
                    }
                }
                .disabled(!enabled || !page.inventory.qualifiedSources)
                .accessibilityLabel("Show the enclosing Markdown for a displayed element")
            }.padding(8)
            ScrollView([.horizontal, .vertical]) {
                DisplayCanvas(page: page, fonts: fonts, palette: appearance,
                    enabled: enabled, openSource: openSource)
                    .frame(width: max(160, page.inventory.bounds.width) + 32,
                           height: max(48, page.bottom - page.top) + 32)
                    .accessibilityRepresentation {
                        VStack(alignment: .leading) {
                            ForEach(page.items) { item in
                                if let text = item.readingText {
                                    if item.original != nil {
                                        Button(text) { openSource(item) }
                                            .disabled(!enabled)
                                            .accessibilityHint("Shows the enclosing captured Markdown")
                                    } else { Text(verbatim: text) }
                                }
                            }
                        }
                    }
            }
            .id(page.id)
            .background(Color(nsColor: appearance.background.nsColor))
        }
    }
}

@MainActor private struct DisplayCanvas: NSViewRepresentable {
    let page: AtlasDocumentDisplayPage
    let fonts: [AtlasDocumentDisplayFont]
    let palette: AtlasAppearancePalette
    let enabled: Bool
    let openSource: (AtlasDocumentDisplayItem) -> Void

    func makeNSView(context: Context) -> DisplayCanvasView { DisplayCanvasView() }
    func updateNSView(_ view: DisplayCanvasView, context: Context) {
        view.install(page, fonts: fonts, palette: palette, enabled: enabled, openSource: openSource)
    }
}

@MainActor private final class DisplayCanvasView: NSView {
    private struct FontKey: Hashable { let index: UInt64; let size: Double }
    private struct GlyphBatch {
        let font: CTFont
        let origin: CGPoint
        let glyphs: [CGGlyph]
        let positions: [CGPoint]
        let role: String
    }
    private enum Drawing {
        case glyphs(GlyphBatch)
        case vector(AtlasDocumentDisplayRect, shape: String, role: String, stroke: Double)
        case image(AtlasDocumentDisplayRect, label: CTLine)
        case raster(AtlasDocumentDisplayRect, resource: AtlasDocumentDisplayImage)
    }
    private var page: AtlasDocumentDisplayPage?
    private var drawings: [Drawing] = []
    private var palette = AtlasAppearancePalette.light
    private var enabled = false
    private var selection: UInt64?
    private var openSource: ((AtlasDocumentDisplayItem) -> Void)?
    override var isFlipped: Bool { true }
    override var isOpaque: Bool { true }

    func install(_ page: AtlasDocumentDisplayPage, fonts: [AtlasDocumentDisplayFont],
                 palette: AtlasAppearancePalette, enabled: Bool,
                 openSource: @escaping (AtlasDocumentDisplayItem) -> Void) {
        let changed = self.page?.id != page.id || self.page?.imageRevision != page.imageRevision
        self.page = page; self.palette = palette; self.enabled = enabled; self.openSource = openSource
        if changed {
            selection = nil; drawings = []
            var nativeFonts: [FontKey: CTFont] = [:]
            func font(_ index: UInt64, size: Double) -> CTFont? {
                let key = FontKey(index: index, size: size)
                if let cached = nativeFonts[key] { return cached }
                guard let source = fonts.first(where: { $0.index == index }) else { return nil }
                let result = CTFontCreateWithGraphicsFont(source.face, CGFloat(size), nil, nil)
                nativeFonts[key] = result
                return result
            }
            for item in page.items {
                switch item.content {
                case .text(_, let role, let size, let glyphs):
                    guard let first = glyphs.first, let baseFont = font(first.font, size: size) else { continue }
                    let ascent = Double(CTFontGetAscent(baseFont)), descent = Double(CTFontGetDescent(baseFont))
                    // Upstream supplies a top-left line box. Its shaped offsets
                    // are relative to a typographic baseline, with positive Y up.
                    let origin = CGPoint(x: item.bounds.x,
                        y: item.bounds.y + (item.bounds.height - ascent - descent) / 2 + ascent)
                    var start = 0
                    while start < glyphs.count {
                        let index = glyphs[start].font
                        var end = start + 1
                        while end < glyphs.count && glyphs[end].font == index { end += 1 }
                        if let face = font(index, size: size) {
                            let run = glyphs[start..<end]
                            drawings.append(.glyphs(.init(font: face, origin: origin,
                                glyphs: run.map(\.id), positions: run.map { CGPoint(x: $0.x, y: $0.y) }, role: role)))
                        }
                        start = end
                    }
                case .vector(let shape, let role, let stroke):
                    drawings.append(.vector(item.bounds, shape: shape, role: role, stroke: stroke))
                case .image(let alt):
                    if let image = page.image(for: item) {
                        // Keep the wrapper, not a borrowed CGImage. Its bounded
                        // pixel reservation survives drawing and retires off-thread.
                        drawings.append(.raster(item.bounds, resource: image))
                        continue
                    }
                    // Placeholder decoration is prepared once, never shaped
                    // during scrolling. Full alt text remains accessible.
                    let label = alt.isEmpty ? "Image unavailable" : "Image: " + String(alt.prefix(512))
                    let text = NSAttributedString(string: label, attributes: [
                        .font: NSFont.systemFont(ofSize: 12),
                        NSAttributedString.Key(kCTForegroundColorFromContextAttributeName as String): true
                    ])
                    drawings.append(.image(item.bounds, label: CTLineCreateWithAttributedString(text)))
                case .anchor: break
                }
            }
        }
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        guard let context = NSGraphicsContext.current?.cgContext, let page else { return }
        context.setFillColor(palette.background.cgColor)
        context.fill(bounds)
        context.saveGState()
        context.translateBy(x: 16 - page.inventory.bounds.x, y: 16 - page.top)
        for drawing in drawings {
            switch drawing {
            case .glyphs(let run):
                context.saveGState()
                context.setFillColor(color(run.role).cgColor)
                context.translateBy(x: run.origin.x, y: run.origin.y)
                context.scaleBy(x: 1, y: -1)
                // Core Graphics does not save/restore textMatrix with gstate.
                context.textMatrix = .identity
                CTFontDrawGlyphs(run.font, run.glyphs, run.positions, run.glyphs.count, context)
                context.restoreGState()
            case .vector(let box, let shape, let role, let stroke):
                drawVector(box, shape: shape, role: role, stroke: stroke, context: context)
            case .raster(let box, let resource):
                resource.draw(in: context, slot: box)
            case .image(let box, let label):
                let rect = rectangle(box)
                context.saveGState()
                context.setStrokeColor(color("muted").cgColor)
                context.setLineDash(phase: 0, lengths: [5, 4])
                context.stroke(rect.insetBy(dx: 0.5, dy: 0.5))
                context.restoreGState()
                context.saveGState()
                context.clip(to: rect.insetBy(dx: 6, dy: 4))
                context.setFillColor(color("muted").cgColor)
                context.translateBy(x: rect.minX + 6, y: rect.midY + 4)
                context.scaleBy(x: 1, y: -1)
                context.textMatrix = .identity
                context.textPosition = .zero
                CTLineDraw(label, context)
                context.restoreGState()
            }
        }
        if let selection, let item = page.items.first(where: { $0.id == selection }) {
            context.setStrokeColor(palette.selection.cgColor)
            context.setLineWidth(palette.increasedContrast ? 2 : 1)
            context.stroke(rectangle(item.bounds).insetBy(dx: -2, dy: -2))
        }
        context.restoreGState()
    }
    private func rectangle(_ rect: AtlasDocumentDisplayRect) -> CGRect {
        CGRect(x: rect.x, y: rect.y, width: rect.width, height: rect.height)
    }
    private func color(_ role: String) -> AtlasAppearanceColor {
        switch role {
        case "link": palette.color("type")
        case "muted", "quote", "rule", "border", "table-border": palette.color("comment")
        default: palette.color(role)
        }
    }
    private func drawVector(_ box: AtlasDocumentDisplayRect, shape: String, role: String,
                            stroke: Double, context: CGContext) {
        let rect = rectangle(box)
        context.saveGState()
        context.setStrokeColor(color(role).cgColor)
        context.setLineWidth(max(palette.increasedContrast ? 1.5 : 0.5, stroke))
        switch shape {
        case "horizontal-rule":
            context.move(to: CGPoint(x: rect.minX, y: rect.midY))
            context.addLine(to: CGPoint(x: rect.maxX, y: rect.midY))
        case "quote-accent":
            context.move(to: CGPoint(x: rect.midX, y: rect.minY))
            context.addLine(to: CGPoint(x: rect.midX, y: rect.maxY))
        case "checkbox-check":
            context.move(to: CGPoint(x: rect.minX, y: rect.midY))
            context.addLine(to: CGPoint(x: rect.midX, y: rect.maxY))
            context.addLine(to: CGPoint(x: rect.maxX, y: rect.minY))
        default: context.addRect(rect)
        }
        context.strokePath()
        context.restoreGState()
    }
    override func mouseUp(with event: NSEvent) {
        guard enabled, let page else { return }
        let point = convert(event.locationInWindow, from: nil)
        let sourcePoint = CGPoint(x: point.x - 16 + page.inventory.bounds.x, y: point.y - 16 + page.top)
        guard let item = page.items.reversed().first(where: {
            $0.original != nil && $0.readingText != nil && rectangle($0.bounds).contains(sourcePoint)
        }) else { return }
        selection = item.id; needsDisplay = true
        openSource?(item)
    }
}
