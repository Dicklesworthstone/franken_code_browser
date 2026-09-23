#!/bin/sh
# Package a built FrankenCodeBrowser.app as a drag-to-Applications image.
# All staging and output paths are new; retained staging permits inspection.
set -eu

usage() {
    cat <<'USAGE'
Usage: scripts/package_macos_dmg.sh --app APP --output ABSOLUTE_PATH.dmg [--identity DEVELOPER_ID] (--notary-profile PROFILE | --local-test)

--notary-profile submits the DMG to Apple, waits for acceptance, and staples it.
--local-test creates an unnotarized test image; do not publish it as an installer.
The output and its .staging sibling must not already exist.
USAGE
}

app=''
output=''
identity=''
profile=''
local_test=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --app|--output|--identity|--notary-profile)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
            case "$1" in
                --app) app=$2 ;;
                --output) output=$2 ;;
                --identity) identity=$2 ;;
                --notary-profile) profile=$2 ;;
            esac
            shift 2 ;;
        --local-test) local_test=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

[ -n "$app" ] && [ -n "$output" ] || { usage >&2; exit 2; }
case "$output" in /*.dmg) ;; *) echo 'output must be an absolute .dmg path' >&2; exit 2 ;; esac
if [ "$local_test" -eq 1 ]; then
    [ -z "$profile" ] || { echo 'choose --local-test or --notary-profile' >&2; exit 2; }
else
    [ -n "$profile" ] && [ -n "$identity" ] || { echo 'public packaging needs a Developer ID identity and notarization profile' >&2; exit 2; }
fi
[ -d "$app/Contents/MacOS" ] && [ -f "$app/Contents/Info.plist" ] || { echo 'invalid .app bundle' >&2; exit 2; }
[ ! -e "$output" ] || { echo "output already exists: $output" >&2; exit 2; }
staging=${output%.dmg}.staging
[ ! -e "$staging" ] || { echo "staging already exists: $staging" >&2; exit 2; }
[ -d "$(dirname "$output")" ] || { echo 'output parent does not exist' >&2; exit 2; }

mkdir "$staging"
cp -R "$app" "$staging/FrankenCodeBrowser.app"
ln -s /Applications "$staging/Applications"

if [ -n "$identity" ]; then
    codesign --force --options runtime --timestamp --sign "$identity" "$staging/FrankenCodeBrowser.app"
fi
codesign --verify --deep --strict --verbose=2 "$staging/FrankenCodeBrowser.app"
hdiutil create -volname FrankenCodeBrowser -srcfolder "$staging" -format UDZO -imagekey zlib-level=9 "$output"
hdiutil verify "$output"

if [ "$local_test" -eq 0 ]; then
    xcrun notarytool submit "$output" --keychain-profile "$profile" --wait
    xcrun stapler staple "$output"
    xcrun stapler validate "$output"
    echo 'Notarized DMG ready for Gatekeeper qualification.'
else
    echo 'LOCAL TEST ONLY: this DMG is not notarized or ready for public distribution.' >&2
fi
shasum -a 256 "$output"
echo "Staging retained for inspection: $staging"
