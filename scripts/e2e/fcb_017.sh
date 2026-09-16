#!/bin/bash
# FCB-017 production verification scenario: Glyph atlas, raster queues,
# and safe resource retirement.
#
# Supported route:
#   scripts/e2e/fcb_017.sh [cargo-test-args...]
#
# Environment:
#   FCB_017_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_017_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_017_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-017/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

echo "[fcb-017] route: headless cargo test (fcb/glyph_atlas_test + fcb/raster_queue_and_churn_test + fcb/fcb_017_production)"
echo "[fcb-017] run id: $RUN_ID"

cd "$REPO_ROOT/crates/fcb"
cargo test --test glyph_atlas_test --test raster_queue_and_churn_test --test fcb_017_production "$@"
cd "$REPO_ROOT"

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-017-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-017] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-017] PASS (all required routes executed)"
