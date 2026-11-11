import Foundation

/// Compile with AtlasAppearance.swift through the strict RCH lane. These
/// portable palette contracts do not qualify native text or Metal rendering.
@main struct AtlasAppearanceTests {
    static func luminance(_ color: AtlasAppearanceColor) -> Double {
        func linear(_ component: Double) -> Double {
            component <= 0.04045 ? component / 12.92 : pow((component + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * linear(color.red) + 0.7152 * linear(color.green) + 0.0722 * linear(color.blue)
    }
    static func contrast(_ a: AtlasAppearanceColor, _ b: AtlasAppearanceColor) -> Double {
        let x = luminance(a), y = luminance(b)
        return (max(x, y) + 0.05) / (min(x, y) + 0.05)
    }
    static func main() {
        precondition(AtlasAppearancePreference.allCases.map(\.rawValue) == ["system", "light", "dark"])
        precondition(AtlasAppearancePalette.resolve(dark: false, increasedContrast: false) == .light)
        precondition(AtlasAppearancePalette.resolve(dark: true, increasedContrast: false) == .dark)
        precondition(AtlasAppearancePalette.resolve(dark: false, increasedContrast: true) == .highContrastLight)
        precondition(AtlasAppearancePalette.resolve(dark: true, increasedContrast: true) == .highContrastDark)
        let canonical = AtlasAppearancePalette.canonical
        let roles = ["plain", "keyword", "type", "function", "string", "number", "comment"]
        let custom = AtlasAppearanceColor(0x123456, alpha: 0.4)
        for palette in AtlasAppearancePalette.allCases {
            precondition(palette.replacing(custom) == custom, "unknown presentation colors stay intact")
            precondition(palette.replacing(canonical.background) == palette.background)
            for role in roles {
                let source = canonical.color(role).withAlpha(0.6)
                precondition(palette.replacing(source) == palette.color(role).withAlpha(0.6),
                             "retained canonical roles map without losing opacity")
                if !palette.isCanonical {
                    precondition(contrast(palette.color(role), palette.background) >= (palette.increasedContrast ? 7 : 4.5),
                                 "every syntax role, including comments, must remain readable")
                }
            }
            precondition(palette.color("operator") == palette.color("keyword"))
            precondition(palette.color("func") == palette.color("function"))
            precondition(palette.color("str") == palette.color("string"))
            precondition(palette.selection != palette.search)
        }
        print("AtlasAppearance: host resolution, canonical role mapping, alpha preservation and contrast contracts passed")
    }
}
