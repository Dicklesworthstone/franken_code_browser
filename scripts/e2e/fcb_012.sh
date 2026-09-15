#!/bin/bash
# FCB-012.V production verification scenario: sparse line indexes and
# encoding maps driven together (see crates/fcb-source/tests/
# fcb_012_production.rs for the required-case registry).
#
# Supported route (documented invocation):
#   scripts/e2e/fcb_012.sh [cargo-test-args...]
#
# Environment:
#   FCB_012_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-012-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-012/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_012_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_012_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-012/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

echo "[fcb-012] route: headless cargo test (fcb-source/fcb_012_production)"
echo "[fcb-012] run id: $RUN_ID"

cargo test --manifest-path crates/fcb-source/Cargo.toml \
    --test fcb_012_production "$@"

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-012-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-012] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-012] PASS (all required routes executed)"
