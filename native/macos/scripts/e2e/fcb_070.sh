#!/bin/sh
# FCB-070.V joint verification campaign: Safe native embedding and
# host-owned loop/device/presentation binding.
#
# Exercises BOTH children together:
#   - FCB-070.A: safe host-owned native view binding (CAMetalLayer bound
#     to a host window with exactly one presentation owner).
#   - FCB-070.B: attach/detach and host lifecycle isolation (no NSApplication
#     takeover, no conflicting class registration, no private cache sharing).
#
# Supported route: NATIVE Apple Silicon execution. This crate binds AppKit
# and Metal; Linux workers cannot compile or run it. The script therefore
# requires an admitted native macOS execution lane; do not bypass the configured Cargo shim.
#
# Exact invocation from the repository toplevel:
#   ./native/macos/scripts/e2e/fcb_070.sh
set -eu
cd "$(dirname "$0")/../.."
echo "=== FCB-070.V campaign: host-owned view binding + lifecycle isolation ==="
exec cargo test --manifest-path Cargo.toml --test view_binding -- --nocapture
