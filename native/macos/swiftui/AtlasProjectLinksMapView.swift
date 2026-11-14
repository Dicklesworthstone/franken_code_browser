import SwiftUI

/// A frozen, selected-file view of direct evidence over existing atlas parcels.
/// The evidence list remains the accessible equivalent for every connection,
/// including occurrences withheld by the map's fixed presentation budget.
@MainActor struct AtlasProjectLinksMapView: View {
    let neighborhood: AtlasProjectLinksNeighborhood
    let projection: AtlasProjectRelationshipProjection
    let select: ([UInt8]) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            GeometryReader { viewport in
                let transform = Transform(bounds: neighborhood.bounds, viewport: viewport.size)
                ZStack {
                    ForEach(neighborhood.connections) { connection in
                        if let source = neighborhood.nodes.first(where: { $0.id == connection.source }),
                           let target = neighborhood.nodes.first(where: { $0.id == connection.target }) {
                            AtlasProjectLinkArrow(source: transform.rect(source.parcel), target: transform.rect(target.parcel),
                                selfLink: connection.source == connection.target, count: connection.count,
                                color: connection.target == neighborhood.selectedFile ? .blue : .orange)
                        }
                    }
                    ForEach(neighborhood.nodes) { node in
                        let rect = transform.rect(node.parcel)
                        let selected = node.id == neighborhood.selectedFile
                        Button { select(node.path.bytes) } label: {
                            RoundedRectangle(cornerRadius: 3)
                                .fill((selected ? Color.accentColor : Color.secondary).opacity(selected ? 0.22 : 0.10))
                                .overlay {
                                    RoundedRectangle(cornerRadius: 3)
                                        .strokeBorder(selected ? Color.accentColor : Color.secondary, lineWidth: selected ? 2 : 1)
                                }
                                .frame(width: rect.width, height: rect.height)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .position(x: rect.midX, y: rect.midY)
                        .help(node.path.display + (selected ? " · selected file" : " · inspect direct relationships"))
                        .accessibilityLabel(Text(verbatim: node.path.display))
                        .accessibilityValue(selected ? "Selected file" : "Related catalog file")
                        .accessibilityHint("Show incoming and outgoing \(projection.noun) for this file")
                    }
                }
                .frame(width: viewport.size.width, height: viewport.size.height)
                .clipped()
                .background(Color.secondary.opacity(0.035), in: RoundedRectangle(cornerRadius: 6))
            }
            .frame(height: 200)
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Direct \(projection.noun) map")
            HStack(spacing: 12) {
                Label("Outgoing", systemImage: "arrow.up.right").foregroundStyle(.orange)
                Label(projection.directionLabel(.incoming), systemImage: "arrow.down.left").foregroundStyle(.blue)
                Spacer()
                Text("×N = evidence records").foregroundStyle(.secondary)
            }.font(.caption2)
            Text(neighborhood.summary).font(.caption2).foregroundStyle(.secondary).textSelection(.enabled)
            Text("Select a parcel to follow its direct relationships. Positions belong to this captured project's atlas; only this result page is drawn.")
                .font(.caption2).foregroundStyle(.secondary)
        }
    }

    private struct Transform {
        let bounds: AtlasProjectLinkParcel
        let scale, offsetX, offsetY: Double
        init(bounds: AtlasProjectLinkParcel, viewport: CGSize) {
            self.bounds = bounds
            let width = max(1, Double(viewport.width)), height = max(1, Double(viewport.height))
            scale = min(max(1, width - 48) / max(1, bounds.width), max(1, height - 48) / max(1, bounds.height))
            offsetX = (width - bounds.width * scale) / 2
            offsetY = (height - bounds.height * scale) / 2
        }
        func rect(_ parcel: AtlasProjectLinkParcel) -> CGRect {
            let width = max(12, parcel.width * scale), height = max(12, parcel.height * scale)
            let midX = (parcel.x + parcel.width / 2 - bounds.x) * scale + offsetX
            let midY = (parcel.y + parcel.height / 2 - bounds.y) * scale + offsetY
            return CGRect(x: midX - width / 2, y: midY - height / 2, width: width, height: height)
        }
    }
}

private struct AtlasProjectLinkArrow: View {
    let source, target: CGRect
    let selfLink: Bool
    let count: Int
    let color: Color

    var body: some View {
        let route = Route(source: source, target: target, selfLink: selfLink)
        ZStack {
            route.path.stroke(color.opacity(0.8), style: StrokeStyle(lineWidth: 1.5, lineCap: .round))
            route.arrow.fill(color)
            if count > 1 {
                Text("×\(count)").font(.caption2.monospacedDigit()).foregroundStyle(color)
                    .padding(.horizontal, 3).background(.regularMaterial, in: Capsule())
                    .position(route.label)
            }
        }
        .allowsHitTesting(false).accessibilityHidden(true)
    }

    /// Deterministic screen-space decoration; existing world parcels do not move.
    private struct Route {
        let path, arrow: Path
        let label: CGPoint
        init(source: CGRect, target: CGRect, selfLink: Bool) {
            let a = CGPoint(x: source.midX, y: source.midY), b = CGPoint(x: target.midX, y: target.midY)
            let dx = b.x - a.x, dy = b.y - a.y, distance = hypot(dx, dy)
            let end: CGPoint, tangent: CGPoint
            if selfLink || distance < 1 {
                let start = CGPoint(x: source.maxX, y: source.midY)
                end = CGPoint(x: target.midX, y: target.minY)
                let first = CGPoint(x: source.maxX + 22, y: source.minY - 22)
                let second = CGPoint(x: target.midX + 22, y: target.minY - 22)
                path = Path { $0.move(to: start); $0.addCurve(to: end, control1: first, control2: second) }
                tangent = CGPoint(x: end.x - second.x, y: end.y - second.y)
                label = CGPoint(x: source.maxX + 12, y: source.minY - 12)
            } else {
                func boundary(_ center: CGPoint, rect: CGRect, sign: CGFloat) -> CGPoint {
                    let ratio = min(0.45, 1 / max(abs(dx) / (rect.width / 2 + 2), abs(dy) / (rect.height / 2 + 2)))
                    return CGPoint(x: center.x + dx * ratio * sign, y: center.y + dy * ratio * sign)
                }
                let start = boundary(a, rect: source, sign: 1)
                end = boundary(b, rect: target, sign: -1)
                let bend = min(24, distance * 0.18)
                let control = CGPoint(x: (start.x + end.x) / 2 - dy / distance * bend,
                    y: (start.y + end.y) / 2 + dx / distance * bend)
                path = Path { $0.move(to: start); $0.addQuadCurve(to: end, control: control) }
                tangent = CGPoint(x: end.x - control.x, y: end.y - control.y)
                label = CGPoint(x: (start.x + end.x + 2 * control.x) / 4,
                    y: (start.y + end.y + 2 * control.y) / 4)
            }
            let length = max(1, hypot(tangent.x, tangent.y))
            let ux = tangent.x / length, uy = tangent.y / length
            arrow = Path {
                $0.move(to: end)
                $0.addLine(to: CGPoint(x: end.x - ux * 7 - uy * 3, y: end.y - uy * 7 + ux * 3))
                $0.addLine(to: CGPoint(x: end.x - ux * 7 + uy * 3, y: end.y - uy * 7 - ux * 3))
                $0.closeSubpath()
            }
        }
    }
}
