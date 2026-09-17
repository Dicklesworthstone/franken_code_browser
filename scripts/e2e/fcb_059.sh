#!/bin/bash
# FCB-059 Hostile corpus and regression minimizer scenario runner (HOSTILE.source lane).
#
# Supported route (documented invocation):
#   scripts/e2e/fcb_059.sh [cargo-test-args...]
#
# Environment:
#   FCB_059_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-059-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-059/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_059_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_059_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-059/$RUN_ID"
mkdir -p "$ARTIFACT_DIR/receipts"
export FCB_RECEIPTS_DIR="$REPO_ROOT/$ARTIFACT_DIR/receipts"

echo "[fcb-059] route: headless cargo test (fcb-conformance/hostile_source_lane)"
echo "[fcb-059] run id: $RUN_ID"

cargo test --manifest-path crates/fcb-conformance/Cargo.toml \
    --test hostile_source_lane "$@"

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-059-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-059] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-059] PASS (HOSTILE.source regression lane verified)"
