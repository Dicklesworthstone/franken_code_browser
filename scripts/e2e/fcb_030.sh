#!/bin/bash
# FCB-030.V production verification scenario: source-specific structural outlines,
# evidence capability schema, and Inspector facts driven together.
#
# Supported route (documented invocation):
#   scripts/e2e/fcb_030.sh [cargo-test-args...]
#
# Environment:
#   FCB_030_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-030-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-030/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_030_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_030_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-030/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

echo "[fcb-030] route: headless cargo test (fcb-analysis/fcb_030_production)"
echo "[fcb-030] run id: $RUN_ID"

cargo test --manifest-path crates/fcb-analysis/Cargo.toml \
    --test fcb_030_production "$@"

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-030-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-030] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-030] PASS (all required routes executed)"
