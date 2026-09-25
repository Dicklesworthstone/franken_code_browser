#!/bin/sh
# Fail-closed release gate for a direct-download FrankenCodeBrowser DMG.
# Run it on the exact file before uploading it to a release; a nonzero exit
# means the image must not be published as Developer ID-signed and notarized.
# Read-only: the image is only attached read-only, and nothing is launched.
set -u

usage() {
    cat <<'USAGE'
Usage: scripts/verify_macos_dmg.sh [--team TEAM_ID] PATH.dmg

Checks, and exits nonzero if any fails:
  1. the disk image itself carries a valid Developer ID Application signature;
  2. a notarization ticket is stapled to the image;
  3. Gatekeeper accepts the image (spctl --type open, primary signature);
  4. the enclosed FrankenCodeBrowser.app has a valid Developer ID signature
     with hardened runtime from the same team, and Gatekeeper accepts it.
--team additionally requires that Team ID on both signatures.
USAGE
}

team=''
dmg=''
while [ "$#" -gt 0 ]; do
    case "$1" in
        --team)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
            team=$2
            shift 2 ;;
        -h|--help) usage; exit 0 ;;
        -*) usage >&2; exit 2 ;;
        *)
            [ -z "$dmg" ] || { usage >&2; exit 2; }
            dmg=$1
            shift ;;
    esac
done
[ -n "$dmg" ] || { usage >&2; exit 2; }
[ -f "$dmg" ] || { echo "not a file: $dmg" >&2; exit 2; }
for tool in codesign spctl xcrun hdiutil; do
    command -v "$tool" >/dev/null 2>&1 || { echo "$tool is required" >&2; exit 2; }
done

failures=0
fail() {
    echo "FAIL: $*" >&2
    failures=$((failures + 1))
}
pass() {
    echo "ok: $*"
}

# Prints the TeamIdentifier of a Developer ID Application signature, or fails.
developer_id_team() {
    details=$(codesign -dv --verbose=4 "$1" 2>&1) || return 1
    printf '%s\n' "$details" | grep -q '^Authority=Developer ID Application: ' || return 1
    printf '%s\n' "$details" | sed -n 's/^TeamIdentifier=//p' | head -n 1
}

# 1. Signature on the container itself.
dmg_team=''
if codesign --verify --strict --verbose=2 "$dmg" >/dev/null 2>&1 \
    && dmg_team=$(developer_id_team "$dmg") && [ -n "$dmg_team" ] && [ "$dmg_team" != 'not set' ]; then
    pass "image is Developer ID-signed (team $dmg_team)"
else
    fail "image has no valid Developer ID Application signature: $(codesign -dv "$dmg" 2>&1 | head -n 1)"
    dmg_team=''
fi
if [ -n "$team" ] && [ -n "$dmg_team" ] && [ "$dmg_team" != "$team" ]; then
    fail "image Team ID $dmg_team does not match expected $team"
fi

# 2. Stapled notarization ticket.
if xcrun stapler validate "$dmg" >/dev/null 2>&1; then
    pass 'notarization ticket is stapled to the image'
else
    fail 'no valid stapled notarization ticket on the image'
fi

# 3. Gatekeeper assessment of the image.
assessment=$(spctl -a -t open --context context:primary-signature -v "$dmg" 2>&1)
if [ "$?" -eq 0 ] && printf '%s\n' "$assessment" | grep -q '^source=Notarized Developer ID$'; then
    pass 'Gatekeeper accepts the image as Notarized Developer ID'
else
    fail "Gatekeeper does not accept the image as Notarized Developer ID: $(printf '%s' "$assessment" | tr '\n' ' ')"
fi

# 4. The enclosed app, attached read-only at a random mount point.
mountpoint=''
detach() {
    if [ -n "$mountpoint" ]; then
        hdiutil detach -quiet "$mountpoint" || hdiutil detach -quiet -force "$mountpoint" || true
        mountpoint=''
    fi
}
trap detach EXIT
trap 'detach; exit 130' INT TERM
attach_output=$(hdiutil attach -readonly -nobrowse -noautoopen -noverify \
    -mountrandom "${TMPDIR:-/tmp}" "$dmg" 2>&1) \
    || { fail "could not attach image: $attach_output"; attach_output=''; }
mountpoint=$(printf '%s\n' "$attach_output" | awk -F '\t' '$NF ~ /^\// { mp = $NF } END { print mp }' | sed 's/[[:space:]]*$//')
app="$mountpoint/FrankenCodeBrowser.app"
if [ -z "$mountpoint" ]; then
    fail 'image has no mountable volume'
elif [ ! -d "$app" ]; then
    fail 'image does not contain FrankenCodeBrowser.app'
else
    app_team=''
    if codesign --verify --deep --strict --verbose=2 "$app" >/dev/null 2>&1 \
        && app_team=$(developer_id_team "$app") && [ -n "$app_team" ]; then
        pass "app is Developer ID-signed (team $app_team)"
    else
        fail 'app has no valid Developer ID Application signature'
        app_team=''
    fi
    if codesign -dv "$app" 2>&1 | grep -q '^CodeDirectory .*flags=0x[0-9a-f]*([^)]*runtime'; then
        pass 'app uses the hardened runtime'
    else
        fail 'app is not signed with the hardened runtime'
    fi
    if [ -n "$app_team" ] && [ -n "$dmg_team" ] && [ "$app_team" != "$dmg_team" ]; then
        fail "app Team ID $app_team differs from image Team ID $dmg_team"
    fi
    if [ -n "$team" ] && [ -n "$app_team" ] && [ "$app_team" != "$team" ]; then
        fail "app Team ID $app_team does not match expected $team"
    fi
    app_assessment=$(spctl -a -t execute -v "$app" 2>&1)
    if [ "$?" -eq 0 ] && printf '%s\n' "$app_assessment" | grep -q '^source=Notarized Developer ID$'; then
        pass 'Gatekeeper accepts the app as Notarized Developer ID'
    else
        fail "Gatekeeper does not accept the app as Notarized Developer ID: $(printf '%s' "$app_assessment" | tr '\n' ' ')"
    fi
fi
detach

if [ "$failures" -ne 0 ]; then
    echo "REJECTED: $dmg failed $failures release check(s); do not publish it as a Developer ID-signed, notarized DMG." >&2
    exit 1
fi
echo "VERIFIED: $dmg is a Developer ID-signed, notarized and stapled DMG containing a Developer ID-signed, notarized app."
