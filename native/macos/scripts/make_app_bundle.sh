#!/bin/sh
# Builds FrankenCodeBrowser.app: SwiftUI shell + Rust engine (staticlib).
# Requires: libfcb_bridge.a built from this workspace and Xcode swiftc.
set -eu
cd "$(dirname "$0")/.."
case "$(uname -m)" in
    arm64) HOST_TARGET=aarch64-apple-darwin ;;
    x86_64) HOST_TARGET=x86_64-apple-darwin ;;
    *) echo 'unsupported macOS architecture' >&2; exit 2 ;;
esac
BRIDGE="${FCB_BRIDGE_LIB:-../../target/$HOST_TARGET/release/libfcb_bridge.a}"
APP="${FCB_APP_OUTPUT:-target/FrankenCodeBrowser.app}"
SWIFT_TARGET="${FCB_SWIFT_TARGET:-$(uname -m)-apple-macosx14.0}"
[ -f "$BRIDGE" ] || { echo "missing fcb-bridge archive: $BRIDGE" >&2; exit 2; }
[ ! -e "$APP" ] || { echo "app output already exists: $APP" >&2; exit 2; }
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp swiftui/Resources/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
xcrun swiftc -O -target "$SWIFT_TARGET" -parse-as-library swiftui/AtlasCamera.swift swiftui/AtlasSource.swift swiftui/AtlasSearch.swift swiftui/AtlasMatch.swift swiftui/AtlasDocument.swift swiftui/AtlasPreparedText.swift swiftui/AtlasProjectCache.swift swiftui/AtlasMetalRasterRenderer.swift swiftui/AtlasMetalGlyphRenderer.swift swiftui/AtlasMetalPresentation.swift swiftui/AtlasRetainedView.swift swiftui/AtlasParcelLayout.swift swiftui/App.swift "$BRIDGE" \
    -framework SwiftUI -framework AppKit \
    -o "$APP/Contents/MacOS/FrankenCodeBrowser"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>FrankenCodeBrowser</string>
    <key>CFBundleDisplayName</key><string>FrankenCodeBrowser</string>
    <key>CFBundleIdentifier</key><string>dev.frankencode.browser</string>
    <key>CFBundleExecutable</key><string>FrankenCodeBrowser</string>
    <key>CFBundleIconFile</key><string>AppIcon.icns</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>2</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST
codesign --force --sign - "$APP"
echo "bundle ready: $APP"
echo "launch with:  open $APP"
