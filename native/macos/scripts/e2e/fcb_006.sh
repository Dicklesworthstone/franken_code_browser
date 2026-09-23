#!/bin/sh
# FCB-006.V joint verification campaign: Display-link integration and
# bounded frame ownership (FCB-006.A display-link pacing + FCB-006.B
# demand-driven frame scheduling).
#
# Supported route: NATIVE Apple Silicon execution. This crate binds AppKit
# and Metal; Linux workers cannot compile or run it. The script therefore
# requires an admitted native macOS execution lane; do not bypass the configured Cargo shim.
#
# Exact invocation from the repository toplevel:
#   ./scripts/e2e/fcb_006.sh
#
# The campaign exercises both layers together:
#   1. Unit & oracle test suites (tests/pacing.rs + tests/scheduler.rs)
#      - Real 60Hz and 120Hz high-refresh pacing traces
#      - Stationary idle detection (zero continuous idle redraw)
#      - Single presentation owner exclusivity and stale owner rejection
#      - Pre-submission camera coalescing & obsolete frame drop
#      - Occlusion pausing with active GPU completion drainage
#      - Display migration / window resize handling
#      - Non-blocking teardown and bounded event rings
#   2. Native host main-thread live qualification (src/bin/main_thread_qualification.rs)
#      - AppKit main thread NSWindow + CAMetalLayer live attachment
#      - Live display-link probe on macOS
#      - Live pacing engine bind with single owner
#      - Two-frame starting policy saturation without GPU allocation
#      - Live frame presentation and in-flight counter verification
#      - Live camera coalescing and occlusion pause on AppKit main thread
set -eu
cd "$(dirname "$0")/../.."

echo "=== FCB-006.V campaign: display-link pacing + frame scheduling ==="
cargo test --manifest-path Cargo.toml --test pacing --test scheduler -- --nocapture

echo "=== FCB-006.V host main-thread live qualification ==="
cargo run --manifest-path Cargo.toml --bin main-thread-qualification
