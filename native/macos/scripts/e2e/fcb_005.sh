#!/bin/sh
# FCB-005.V joint verification campaign: Safe Metal resource/upload/
# submission layer (FCB-005.A owned buffers/textures/uploads + FCB-005.B
# bounded submission and terminal ownership).
#
# Supported route: NATIVE Apple Silicon execution. This crate binds AppKit
# and Metal; Linux workers cannot compile or run it. The script therefore
# requires an admitted native macOS execution lane; do not bypass the configured Cargo shim.
#
# Exact invocation from the repository toplevel:
#   ./scripts/e2e/fcb_005.sh
#
# The campaign runs both test targets together (A owned buffers/leases,
# B bounded submission/terminal ownership) and fails if either fails.
set -eu
cd "$(dirname "$0")/../.."
exec cargo test --manifest-path Cargo.toml --test metal_buffer --test submission -- --nocapture
