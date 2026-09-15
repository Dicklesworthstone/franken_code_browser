#!/bin/bash
# FCB-071 production verification scenario: immutable drawing and interaction
# FramePlan, multi-dimensional generation bundling, presented-frame coherence,
# coordinate domain protection, and delayed-presentation hit-test/accessibility oracle.
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

echo "[fcb-071] route: headless cargo test (fcb/frame_plan_presentation + fcb/fcb_071_production)"
echo "[fcb-071] run id: $RUN_ID"

cd "$REPO_ROOT/crates/fcb"
cargo test --test frame_plan_presentation --test fcb_071_production "$@"
cd "$REPO_ROOT"

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-071-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-071] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-071] PASS (all required routes executed)"
