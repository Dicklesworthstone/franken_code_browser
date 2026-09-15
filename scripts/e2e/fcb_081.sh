#!/bin/bash
# FCB-081.V production verification scenario: owned cache namespaces,
# pinned generations, logical clear revocation, and protected reclamation.
#
# Supported route (documented invocation):
#   scripts/e2e/fcb_081.sh [cargo-test-args...]
#
# Environment:
#   FCB_081_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-081-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-081/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_081_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_081_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-081/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

echo "[fcb-081] route: headless cargo test (fcb-store/fcb_081_production)"
echo "[fcb-081] run id: $RUN_ID"

cd "$REPO_ROOT/crates/fcb-store"
cargo test --test fcb_081_production "$@"
cd "$REPO_ROOT"

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-081-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-081] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-081] PASS (all required routes executed)"
