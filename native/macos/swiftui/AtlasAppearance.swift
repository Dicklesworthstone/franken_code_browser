import Foundation

/// Presentation preferences never change captured source, lexical roles, glyph
/// positions, or the repository layout. Canonical colors are the stable on-disk
/// prepared-text representation; a view chooses its own resolved palette.
enum AtlasAppearancePreference: String, CaseIterable, Identifiable {
    case system, light, dark
    var id: String { rawValue }
    var title: String {
        switch self { case .system: "System"; case .light: "Light"; case .dark: "Dark" }
    }
}

struct AtlasAppearanceColor: Equatable, Sendable {
    let red, green, blue, alpha: Double
    init(_ rgb: UInt32, alpha: Double = 1) {
        red = Double((rgb >> 16) & 255) / 255
        green = Double((rgb >> 8) & 255) / 255
        blue = Double(rgb & 255) / 255
        self.alpha = alpha
    }
    init(red: Double, green: Double, blue: Double, alpha: Double = 1) {
        self.red = red; self.green = green; self.blue = blue; self.alpha = alpha
    }
    func withAlpha(_ alpha: Double) -> Self {
        .init(red: red, green: green, blue: blue, alpha: alpha)
    }
    func matchesRGB(_ other: Self) -> Bool {
        abs(red - other.red) < 0.00001 && abs(green - other.green) < 0.00001 && abs(blue - other.blue) < 0.00001
    }
    var vector: SIMD4<Float> { .init(Float(red), Float(green), Float(blue), Float(alpha)) }
}

enum AtlasAppearancePalette: String, CaseIterable, Sendable {
    /// Existing prepared archives and pixel oracles retain this exact identity.
    case canonical, light, dark, highContrastLight, highContrastDark

    static func resolve(dark: Bool, increasedContrast: Bool) -> Self {
        if increasedContrast { return dark ? .highContrastDark : .highContrastLight }
        return dark ? .dark : .light
    }
    var isCanonical: Bool { self == .canonical }
    var isLight: Bool { self == .light || self == .highContrastLight }
    var increasedContrast: Bool { self == .highContrastLight || self == .highContrastDark }
    var background: AtlasAppearanceColor {
        switch self {
        case .canonical, .dark: .init(0x161A1D)
        case .light: .init(0xF8F9FA)
        case .highContrastLight: .init(0xFFFFFF)
        case .highContrastDark: .init(0x000000)
        }
    }
    /// Order is part of the small shader uniform contract, not a lexical ABI.
    /// The shader substitutes only these known canonical presentation colors.
    var syntaxColors: [AtlasAppearanceColor] {
        switch self {
        case .canonical: Self.canonicalColors
        case .dark: Self.darkColors
        case .light: Self.lightColors
        case .highContrastLight: Self.highContrastLightColors
        case .highContrastDark: Self.highContrastDarkColors
        }
    }
    private static let canonicalColors = colors([0xF8F8F2, 0xF92672, 0x66D9EF, 0xA6E22E, 0xE6DB74, 0xAE81FF, 0x75715E])
    private static let darkColors = colors([0xF8F8F2, 0xFF629A, 0x66D9EF, 0xA6E22E, 0xE6DB74, 0xC19AFF, 0xABA893])
    private static let lightColors = colors([0x20262D, 0x9E1957, 0x006778, 0x38650A, 0x806000, 0x6941A8, 0x616050])
    private static let highContrastLightColors = colors([0x000000, 0x790033, 0x004C59, 0x254800, 0x614600, 0x4F2586, 0x49483A])
    private static let highContrastDarkColors = colors([0xFFFFFF, 0xFF91BB, 0x99EBFF, 0xC5F268, 0xFFF09A, 0xD5B7FF, 0xCAC8BA])
    private static func colors(_ values: [UInt32]) -> [AtlasAppearanceColor] { values.map { .init($0) } }
    func color(_ role: String) -> AtlasAppearanceColor {
        let index: Int
        switch role {
        case "keyword", "operator": index = 1
        case "type": index = 2
        case "func", "function": index = 3
        case "str", "string": index = 4
        case "number": index = 5
        case "comment": index = 6
        default: index = 0
        }
        return syntaxColors[index]
    }
    /// Unknown/custom colors remain untouched, including intrinsic color glyphs.
    /// Lookup uses original canonical colors, so repeated switches cannot drift.
    func replacing(_ original: AtlasAppearanceColor) -> AtlasAppearanceColor {
        if isCanonical { return original }
        if original.matchesRGB(Self.canonical.background) { return background.withAlpha(original.alpha) }
        if let index = Self.canonical.syntaxColors.firstIndex(where: { $0.matchesRGB(original) }) {
            return syntaxColors[index].withAlpha(original.alpha)
        }
        return original
    }
    var parcel: AtlasAppearanceColor { isLight ? .init(0xE8EDF2) : .init(increasedContrast ? 0x181818 : 0x21292E) }
    var selection: AtlasAppearanceColor { isLight ? .init(0x004CAE) : .init(0xFFE46B) }
    var search: AtlasAppearanceColor { isLight ? .init(0x874500) : .init(0xFFB454) }
}

#if canImport(AppKit)
import AppKit
import SwiftUI

/// Every native surface resolves the same host setting and explicit override.
/// The system's Increase Contrast setting remains effective even when the
/// application's additional contrast toggle is off.
@propertyWrapper struct AtlasAppearance: DynamicProperty {
    @Environment(\.colorScheme) private var scheme
    @Environment(\.colorSchemeContrast) private var contrast
    @AppStorage("fcb.increasedContrast") private var increasedContrast = false
    var wrappedValue: AtlasAppearancePalette {
        .resolve(dark: scheme == .dark, increasedContrast: increasedContrast || contrast == .increased)
    }
}

extension AtlasAppearancePreference {
    var colorScheme: ColorScheme? {
        switch self { case .system: nil; case .light: .light; case .dark: .dark }
    }
}

extension AtlasAppearanceColor {
    var nsColor: NSColor { .init(srgbRed: red, green: green, blue: blue, alpha: alpha) }
    var cgColor: CGColor { CGColor(srgbRed: red, green: green, blue: blue, alpha: alpha) }
    init?(_ color: CGColor) {
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let converted = color.converted(to: space, intent: .defaultIntent, options: nil),
              let c = converted.components, c.count == 4, c.allSatisfy(\.isFinite) else { return nil }
        self.init(red: Double(c[0]), green: Double(c[1]), blue: Double(c[2]), alpha: Double(c[3]))
    }
}
extension AtlasAppearancePalette {
    func replacing(_ color: CGColor) -> CGColor {
        guard !isCanonical, let value = AtlasAppearanceColor(color) else { return color }
        let mapped = replacing(value)
        return mapped == value ? color : mapped.cgColor
    }
    /// Recolor the existing attributed storage in place. Fonts, paragraph style,
    /// text, selection and scroll anchors are deliberately outside this operation.
    func apply(to storage: NSMutableAttributedString, canonical: NSAttributedString) {
        guard storage.length == canonical.length, storage.string.utf8.elementsEqual(canonical.string.utf8) else { return }
        storage.beginEditing()
        canonical.enumerateAttribute(.foregroundColor, in: NSRange(location: 0, length: canonical.length)) { value, range, _ in
            guard let color = value as? NSColor else { return }
            let mapped = replacing(color.cgColor)
            storage.addAttribute(.foregroundColor,
                value: CFEqual(mapped, color.cgColor) ? color : (NSColor(cgColor: mapped) ?? color), range: range)
        }
        storage.endEditing()
    }
}
#endif
