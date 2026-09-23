#!/bin/sh
# Build the Rust bridge and SwiftUI/Metal app from this single checkout.
set -eu

usage() {
    echo 'Usage: scripts/build_macos_app.sh [--debug] [--output ABSOLUTE_NEW_PATH.app]'
    echo 'Prints the built .app path on stdout; build diagnostics go to stderr.'
}

profile=release
output=''
while [ "$#" -gt 0 ]; do
    case "$1" in
        --debug) profile=debug; shift ;;
        --output) [ "$#" -ge 2 ] || { usage >&2; exit 2; }; output=$2; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
cd "$root"
case "$(uname -s):$(uname -m)" in
    Darwin:arm64) target=aarch64-apple-darwin; swift_target=arm64-apple-macosx14.0 ;;
    Darwin:x86_64) target=x86_64-apple-darwin; swift_target=x86_64-apple-macosx14.0 ;;
    *) echo 'This app build requires macOS on a supported architecture.' >&2; exit 2 ;;
esac
command -v cargo >/dev/null || { echo 'cargo is required' >&2; exit 2; }
command -v xcrun >/dev/null || { echo 'Xcode command-line tools are required' >&2; exit 2; }
if command -v sbh >/dev/null; then sbh check --need 20G >&2; fi

target_dir=${CARGO_TARGET_DIR:-$root/target}
case "$target_dir" in /*) ;; *) target_dir="$root/$target_dir" ;; esac
if [ -z "$output" ]; then
    stamp=$(date -u '+%Y%m%dT%H%M%SZ')
    revision=$(git rev-parse --short=12 HEAD)
    output="$root/dist/FrankenCodeBrowser-$revision-$stamp.app"
fi
case "$output" in /*.app) ;; *) echo 'output must be an absolute .app path' >&2; exit 2 ;; esac
[ ! -e "$output" ] || { echo "app output already exists: $output" >&2; exit 2; }
[ -d "$(dirname "$output")" ] || mkdir -p "$(dirname "$output")"

if [ "$profile" = release ]; then
    cargo build --locked --release -p fcb-bridge --target "$target" >&2
else
    cargo build --locked -p fcb-bridge --target "$target" >&2
fi
bridge="$target_dir/$target/$profile/libfcb_bridge.a"
[ -f "$bridge" ] || { echo "bridge archive missing after build: $bridge" >&2; exit 1; }
FCB_BRIDGE_LIB="$bridge" FCB_APP_OUTPUT="$output" FCB_SWIFT_TARGET="$swift_target" \
    "$root/native/macos/scripts/make_app_bundle.sh" >&2
codesign --verify --deep --strict "$output"
printf '%s\n' "$output"
