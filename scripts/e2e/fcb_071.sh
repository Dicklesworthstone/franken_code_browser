#!/bin/bash
# FCB-071 production verification scenario: immutable drawing and interaction
# FramePlan, multi-dimensional generation bundling, presented-frame coherence,
# and delayed-presentation hit-test/accessibility oracle.
#
# Supported route:
#   scripts/e2e/fcb_071.sh [cargo-test-args...]
#
# Environment:
#   FCB_071_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_071_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_071_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-071/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

echo "[fcb-071] route: headless cargo test (fcb/frame_plan_presentation)"
echo "[fcb-071] run id: $RUN_ID"

cd "$REPO_ROOT/crates/fcb"
cargo test --test frame_plan_presentation "$@"
cd "$REPO_ROOT"

echo "[fcb-071] PASS (all required routes executed)"
