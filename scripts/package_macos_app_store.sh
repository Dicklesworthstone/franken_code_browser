#!/bin/sh
# Sign a sandboxed build for the Mac App Store and produce its upload package.
set -eu

usage() {
    echo 'Usage: scripts/package_macos_app_store.sh --app ABS.app --profile ABS.provisionprofile --signed-app ABS/FrankenCodeBrowser.app --output ABS.pkg --app-identity NAME --installer-identity NAME'
}

app='' profile='' signed_app='' output='' app_identity='' installer_identity=''
while [ "$#" -gt 0 ]; do
    case "$1" in
        --app) app=$2; shift 2 ;;
        --profile) profile=$2; shift 2 ;;
        --signed-app) signed_app=$2; shift 2 ;;
        --output) output=$2; shift 2 ;;
        --app-identity) app_identity=$2; shift 2 ;;
        --installer-identity) installer_identity=$2; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

case "$app:$profile:$signed_app:$output" in
    /*:/*:/*/FrankenCodeBrowser.app:/*.pkg) ;;
    *) usage >&2; exit 2 ;;
esac
[ -d "$app" ] && [ -f "$profile" ] || { echo 'app or profile missing' >&2; exit 2; }
[ -n "$app_identity" ] && [ -n "$installer_identity" ] || { usage >&2; exit 2; }
[ ! -e "$signed_app" ] && [ ! -e "$output" ] || {
    echo 'signed app or output package already exists' >&2; exit 2;
}
signed_entitlements="$signed_app.entitlements.plist"
[ ! -e "$signed_entitlements" ] || {
    echo "signed entitlements already exist: $signed_entitlements" >&2; exit 2;
}
bundle_id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist")
[ "$bundle_id" = dev.frankencode.browser ] || {
    echo "unexpected bundle ID: $bundle_id" >&2; exit 2;
}
codesign -d --entitlements :- "$app" 2>/dev/null |
    /usr/bin/python3 -c 'import plistlib,sys; sys.exit(0 if plistlib.loads(sys.stdin.buffer.read()).get("com.apple.security.app-sandbox") is True else 1)' 2>/dev/null || {
    echo 'app is not signed with the App Sandbox entitlement' >&2; exit 2;
}
profile_id=$(security cms -D -i "$profile" |
    /usr/bin/python3 -c 'import plistlib,sys; print(plistlib.loads(sys.stdin.buffer.read())["Entitlements"]["com.apple.application-identifier"])')
case "$profile_id" in
    *."$bundle_id") ;;
    *) echo "profile does not match bundle ID $bundle_id" >&2; exit 2 ;;
esac

root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
mkdir -p "$(dirname "$signed_app")" "$(dirname "$output")"
ditto "$app" "$signed_app"
cp "$profile" "$signed_app/Contents/embedded.provisionprofile"
security cms -D -i "$profile" | /usr/bin/python3 -c '
import plistlib
import sys

source, destination, bundle_id = sys.argv[1:]
profile = plistlib.load(sys.stdin.buffer)["Entitlements"]
app_id = profile["com.apple.application-identifier"]
team_id = profile["com.apple.developer.team-identifier"]
if app_id != f"{team_id}.{bundle_id}":
    raise SystemExit("profile application identifier does not match team and bundle ID")
with open(source, "rb") as source_file:
    entitlements = plistlib.load(source_file)
entitlements["com.apple.application-identifier"] = app_id
entitlements["com.apple.developer.team-identifier"] = team_id
with open(destination, "xb") as destination_file:
    plistlib.dump(entitlements, destination_file)
' "$root/native/macos/swiftui/AppStore.entitlements" "$signed_entitlements" "$bundle_id"
codesign --force --timestamp --options runtime \
    --entitlements "$signed_entitlements" \
    --sign "$app_identity" "$signed_app"
codesign --verify --deep --strict "$signed_app"
codesign -d --entitlements :- "$signed_app" 2>/dev/null |
    /usr/bin/python3 -c '
import plistlib
import sys

with open(sys.argv[1], "rb") as expected_file:
    expected = plistlib.load(expected_file)
actual = plistlib.load(sys.stdin.buffer)
if actual != expected:
    raise SystemExit("signed entitlements do not match the selected profile")
' "$signed_entitlements"
productbuild --component "$signed_app" /Applications \
    --sign "$installer_identity" "$output"
pkgutil --check-signature "$output"
echo "Mac App Store upload package: $output"
