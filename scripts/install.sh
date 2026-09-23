#!/usr/bin/env bash
# Install the notarized FrankenCodeBrowser Mac app from its GitHub release.
#
# One-line install (cache buster avoids a stale copy of this script):
#   curl -fsSL "https://raw.githubusercontent.com/Dicklesworthstone/franken_code_browser/main/scripts/install.sh?$(date +%s)" | bash
#
# The DMG itself is also a normal drag-to-Applications installer. This script
# installs to ~/Applications without sudo and preserves an old app on upgrades.
set -euo pipefail
umask 022
shopt -s lastpipe 2>/dev/null || true

readonly REPOSITORY='Dicklesworthstone/franken_code_browser'
readonly ASSET='FrankenCodeBrowser-macos-arm64.dmg'
readonly TEAM_ID='AU8V2Z6NKY'
readonly BUNDLE_ID='dev.frankencode.browser'

DEST="$HOME/Applications"
VERSION=''
OFFLINE=''
CHECKSUM_FILE=''
FORCE=0
QUIET=0
NO_GUM=0
HAS_GUM=0
TEMP=''
MOUNT=''
LOCK_DIR=''
LOCKED=0
ATTACHED=0
BACKUP=''
PROXY_ARGS=()

usage() {
    cat <<'USAGE'
Usage: bash install.sh [options]

Download the latest notarized Apple Silicon DMG, verify its SHA-256 and Apple
Developer ID signature, then install FrankenCodeBrowser.app to ~/Applications.

  --version TAG       Install a specific release tag instead of latest
  --dest DIRECTORY    Install into DIRECTORY (default: ~/Applications)
  --offline DMG       Install from a local DMG without network access
  --checksum FILE     SHA-256 sidecar for --offline (default: DMG.sha256)
  --force             Upgrade an existing app, preserving a dated backup
  --quiet             Suppress non-error output
  --no-gum            Use plain output even if gum is installed
  -h, --help          Show this help

The DMG can also be opened in Finder and dragged onto its Applications alias.
Uninstall by moving FrankenCodeBrowser.app from the install directory to Trash.
USAGE
}

while (($#)); do
    case "$1" in
        --version|--dest|--offline|--checksum)
            (($# >= 2)) || { usage >&2; exit 2; }
            case "$1" in
                --version) VERSION=$2 ;;
                --dest) DEST=$2 ;;
                --offline) OFFLINE=$2 ;;
                --checksum) CHECKSUM_FILE=$2 ;;
            esac
            shift 2 ;;
        --force) FORCE=1; shift ;;
        --quiet) QUIET=1; shift ;;
        --no-gum) NO_GUM=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

[[ -z "$VERSION" || "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-.][A-Za-z0-9.-]+)?$ ]] || {
    printf 'Invalid release tag: %s\n' "$VERSION" >&2; exit 2;
}
[[ -z "$CHECKSUM_FILE" || -n "$OFFLINE" ]] || {
    printf '%s\n' '--checksum requires --offline' >&2; exit 2;
}
[[ "$DEST" = /* ]] || { printf '%s\n' '--dest must be an absolute directory' >&2; exit 2; }
[[ "$DEST" != / ]] || { printf '%s\n' 'Refusing to install into /' >&2; exit 2; }

if command -v gum >/dev/null 2>&1 && [[ -t 1 ]]; then HAS_GUM=1; fi
color() {
    local code=$1; shift
    if [[ -t 1 ]]; then printf '\033[%sm%s\033[0m\n' "$code" "$*"; else printf '%s\n' "$*"; fi
}
info() {
    ((QUIET)) && return 0
    if ((HAS_GUM && !NO_GUM)); then gum style --foreground 39 "→ $*"; else color '0;34' "→ $*"; fi
}
ok() {
    ((QUIET)) && return 0
    if ((HAS_GUM && !NO_GUM)); then gum style --foreground 42 "✓ $*"; else color '0;32' "✓ $*"; fi
}
warn() {
    ((QUIET)) && return 0
    if ((HAS_GUM && !NO_GUM)); then gum style --foreground 214 "⚠ $*"; else color '1;33' "⚠ $*"; fi
}
err() { printf '✗ %s\n' "$*" >&2; }
draw_box() {
    ((QUIET)) && return 0
    local line width=0 border='' padding
    for line in "$@"; do
        line=$(printf '%s' "$line" | sed $'s/\033\\[[0-9;]*m//g')
        ((${#line} > width)) && width=${#line}
    done
    printf -v padding '%*s' "$((width + 2))" ''
    border=${padding// /═}
    printf '╔%s╗\n' "$border"
    for line in "$@"; do printf '║ %-*s ║\n' "$width" "$line"; done
    printf '╚%s╝\n' "$border"
}
run_with_spinner() {
    local title=$1; shift
    if ((HAS_GUM && !NO_GUM && !QUIET)); then
        gum spin --spinner dot --title "$title" -- "$@"
    else
        info "$title"
        "$@"
    fi
}

cleanup() {
    local status=$?
    trap - EXIT
    if ((ATTACHED)); then
        hdiutil detach "$MOUNT" -quiet >/dev/null 2>&1 || warn "Detach the DMG at $MOUNT manually"
    fi
    # The test-only retention switch leaves all created files in place when
    # validating this installer in a shared development checkout.
    if [[ ${FCB_INSTALL_KEEP_TEMP:-0} != 1 ]]; then
        if [[ -n "$TEMP" && "$TEMP" == "${TMPDIR:-/tmp}"/fcb-install.* ]]; then rm -rf -- "$TEMP"; fi
        if ((LOCKED)); then rm -f -- "$LOCK_DIR/pid"; rmdir "$LOCK_DIR" 2>/dev/null || true; fi
    fi
    exit "$status"
}
trap cleanup EXIT

[[ $(uname -s) = Darwin && $(uname -m) = arm64 ]] || {
    err 'The downloadable Mac app currently requires Apple Silicon macOS.'; exit 1;
}
major=$(sw_vers -productVersion | cut -d. -f1)
((major >= 14)) || { err 'FrankenCodeBrowser requires macOS 14 or newer.'; exit 1; }
for tool in hdiutil codesign spctl ditto curl; do
    command -v "$tool" >/dev/null 2>&1 || { err "Required macOS tool is missing: $tool"; exit 1; }
done
if command -v sha256sum >/dev/null 2>&1; then
    SHA_TOOL=sha256sum
elif command -v shasum >/dev/null 2>&1; then
    SHA_TOOL=shasum
else
    err 'No SHA-256 tool found (shasum or sha256sum).'; exit 1
fi
if [[ -n ${HTTPS_PROXY:-} ]]; then
    PROXY_ARGS=(--proxy "$HTTPS_PROXY")
elif [[ -n ${HTTP_PROXY:-} ]]; then
    PROXY_ARGS=(--proxy "$HTTP_PROXY")
fi

draw_box 'FrankenCodeBrowser for macOS' 'Verified download · drag-to-Applications DMG'
info 'Checking installation prerequisites'
mkdir -p "$DEST"
[[ -w "$DEST" ]] || { err "No write access to $DEST"; exit 1; }
available=$(df -Pk "$DEST" | awk 'NR == 2 { print $4 }')
if [[ ! "$available" =~ ^[0-9]+$ ]] || ((available < 524288)); then
    err 'At least 512 MiB of free disk space is required.'; exit 1;
fi
LOCK_DIR="$DEST/.frankencodebrowser-install.lock"
if ! mkdir "$LOCK_DIR" 2>/dev/null; then
    old_pid=''
    [[ -f "$LOCK_DIR/pid" ]] && read -r old_pid < "$LOCK_DIR/pid" || true
    if [[ "$old_pid" =~ ^[0-9]+$ ]] && ! kill -0 "$old_pid" 2>/dev/null; then
        warn 'Removing a stale installer lock'
        rm -f -- "$LOCK_DIR/pid"
        rmdir "$LOCK_DIR" 2>/dev/null || { err "Cannot clear lock: $LOCK_DIR"; exit 1; }
        mkdir "$LOCK_DIR" || { err 'Another installation started'; exit 1; }
    else
        err "Another installation may be running: $LOCK_DIR"; exit 1
    fi
fi
LOCKED=1
printf '%s\n' "$$" > "$LOCK_DIR/pid"
TEMP=$(mktemp -d "${TMPDIR:-/tmp}/fcb-install.XXXXXX")
MOUNT="$TEMP/volume"

if [[ -n "$OFFLINE" ]]; then
    [[ -f "$OFFLINE" ]] || { err "DMG not found: $OFFLINE"; exit 1; }
    DMG=$OFFLINE
    SUM_FILE=${CHECKSUM_FILE:-${OFFLINE}.sha256}
    [[ -f "$SUM_FILE" ]] || { err "SHA-256 sidecar not found: $SUM_FILE"; exit 1; }
    info 'Using the supplied offline DMG'
else
    if [[ -n "$VERSION" ]]; then
        base="https://github.com/$REPOSITORY/releases/download/$VERSION"
    else
        base="https://github.com/$REPOSITORY/releases/latest/download"
    fi
    DMG="$TEMP/$ASSET"
    SUM_FILE="$TEMP/$ASSET.sha256"
    info 'Downloading the signed Mac release and checksum'
    curl -fLSs --retry 3 --connect-timeout 15 --max-time 900 "${PROXY_ARGS[@]}" \
        "$base/$ASSET" -o "$DMG" || { err "Could not download $base/$ASSET"; exit 1; }
    curl -fLSs --retry 3 --connect-timeout 15 --max-time 60 "${PROXY_ARGS[@]}" \
        "$base/$ASSET.sha256" -o "$SUM_FILE" || { err 'Could not download the checksum'; exit 1; }
fi

[[ $(wc -l < "$SUM_FILE" | tr -d ' ') = 1 ]] || { err 'Checksum sidecar must contain one line'; exit 1; }
read -r expected filename < "$SUM_FILE"
[[ "$expected" =~ ^[0-9a-fA-F]{64}$ && "$filename" = "$ASSET" ]] || {
    err 'Malformed checksum sidecar'; exit 1;
}
if [[ "$SHA_TOOL" = sha256sum ]]; then actual=$(sha256sum "$DMG" | cut -d' ' -f1)
else actual=$(shasum -a 256 "$DMG" | cut -d' ' -f1); fi
[[ $(printf '%s' "$expected" | tr 'A-F' 'a-f') = "$actual" ]] || {
    err 'DMG SHA-256 mismatch; refusing installation'; exit 1;
}
ok "SHA-256 verified: ${actual:0:16}…"

if ! run_with_spinner 'Verifying the disk image' hdiutil verify "$DMG" -quiet; then
    err 'The disk image failed its internal checksum check'; exit 1
fi
mkdir "$MOUNT"
if ! run_with_spinner 'Mounting the disk image' hdiutil attach "$DMG" -readonly -nobrowse \
    -mountpoint "$MOUNT" -quiet; then
    err 'Could not mount the verified disk image'; exit 1
fi
ATTACHED=1
app="$MOUNT/FrankenCodeBrowser.app"
[[ -d "$app/Contents/MacOS" && ! -L "$app" ]] || { err 'The DMG has no app bundle'; exit 1; }
bundle_id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist")
app_version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")
bundle_version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$app/Contents/Info.plist")
[[ "$bundle_id" = "$BUNDLE_ID" ]] || { err 'Unexpected app bundle identifier'; exit 1; }
codesign --verify --deep --strict "$app" || { err 'App signature is invalid'; exit 1; }
team=$(codesign -dv --verbose=4 "$app" 2>&1 | sed -n 's/^TeamIdentifier=//p')
[[ "$team" = "$TEAM_ID" ]] || { err 'App is not signed by the expected developer'; exit 1; }
spctl --assess --type execute "$app" || { err 'Gatekeeper rejected the app'; exit 1; }
ok "Apple Developer ID and Gatekeeper verified (version $app_version, build $bundle_version)"

destination="$DEST/FrankenCodeBrowser.app"
if [[ -e "$destination" && $FORCE -eq 0 ]]; then
    installed_version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' \
        "$destination/Contents/Info.plist" 2>/dev/null || true)
    installed_short_version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
        "$destination/Contents/Info.plist" 2>/dev/null || true)
    if [[ "$installed_version" = "$bundle_version" && "$installed_short_version" = "$app_version" ]] \
        && codesign --verify --deep --strict "$destination"; then
        ok "Already installed: $destination (version $app_version, build $bundle_version)"
        exit 0
    fi
    err "An app already exists at $destination; use --force to preserve it as a backup and upgrade"
    exit 1
fi

incoming="$DEST/.FrankenCodeBrowser.incoming.$$"
[[ ! -e "$incoming" ]] || { err "Staging path already exists: $incoming"; exit 1; }
run_with_spinner 'Copying the verified app' ditto "$app" "$incoming"
codesign --verify --deep --strict "$incoming" || { err 'Copied app failed signature verification'; exit 1; }
if [[ -e "$destination" ]]; then
    BACKUP="$DEST/FrankenCodeBrowser.backup.$(date -u +%Y%m%dT%H%M%SZ).app"
    [[ ! -e "$BACKUP" ]] || { err "Backup path exists: $BACKUP"; exit 1; }
    mv "$destination" "$BACKUP"
fi
if ! mv "$incoming" "$destination"; then
    [[ -z "$BACKUP" ]] || mv "$BACKUP" "$destination"
    err 'Could not activate the new app; the previous installation was restored'
    exit 1
fi
codesign --verify --deep --strict "$destination"
ok "Installed: $destination"
[[ -z "$BACKUP" ]] || info "Previous app preserved at: $BACKUP"
draw_box "Installed FrankenCodeBrowser $app_version" "Open it from Applications or run: open '$destination'" \
    "To uninstall, move $destination to Trash."
