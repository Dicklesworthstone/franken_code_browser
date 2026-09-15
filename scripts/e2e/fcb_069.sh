#!/bin/bash
# FCB-069 production verification scenario: early semantic accessibility,
# focus state, focus return, finite geometry, and explicit pending ranges.
#
# Supported route:
#   scripts/e2e/fcb_069.sh [cargo-test-args...]
#
# Environment:
#   FCB_069_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_069_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_069_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-069/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

echo "[fcb-069] route: headless cargo test (fcb-core/semantic_focus_geometry)"
echo "[fcb-069] run id: $RUN_ID"

cd "$REPO_ROOT/crates/fcb-core"
cargo test --test semantic_focus_geometry "$@"
cd "$REPO_ROOT"

echo "[fcb-069] PASS (all required routes executed)"
