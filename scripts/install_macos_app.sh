#!/bin/sh
# Build and install, or copy an existing app to the current user's Applications folder.
set -eu
if [ "$#" -ne 1 ]; then
    echo 'Usage: scripts/install_macos_app.sh --build | /absolute/path/FrankenCodeBrowser.app' >&2
    exit 2
fi
destination="$HOME/Applications/FrankenCodeBrowser.app"
[ ! -e "$destination" ] && [ ! -L "$destination" ] || { echo "existing app preserved: $destination" >&2; exit 2; }
if [ "$1" = --build ]; then
    root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
    app=$("$root/scripts/build_macos_app.sh")
else
    app=$1
fi
case "$app" in /*.app) ;; *) echo 'pass an absolute .app path' >&2; exit 2 ;; esac
[ -d "$app/Contents/MacOS" ] || { echo 'app bundle is missing its executable directory' >&2; exit 2; }
codesign --verify --deep --strict "$app"
mkdir -p "$HOME/Applications"
# Atomically claim a new destination after the potentially long build. Never
# merge into an installation which appeared while that build was running.
mkdir "$destination" || { echo "installation destination is no longer available: $destination" >&2; exit 2; }
ditto "$app" "$destination"
codesign --verify --deep --strict "$destination"
echo "Installed: $destination"
if [ -x "$destination/Contents/MacOS/fcb" ]; then
    printf 'CLI: %s\n' "$destination/Contents/MacOS/fcb"
fi
