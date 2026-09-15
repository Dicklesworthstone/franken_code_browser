#!/bin/bash
# FCB-080.V production verification scenario: upstream reusable digest,
# canonical-envelope factoring, and consumer conformance.
#
# Supported route (documented invocation):
#   scripts/e2e/fcb_080.sh [cargo-test-args...]
#
# Environment:
#   FCB_080_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-080-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-080/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_080_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_080_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-080/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

echo "[fcb-080] route: headless cargo test (fcb-store/fcb_080_production)"
echo "[fcb-080] run id: $RUN_ID"

cd "$REPO_ROOT/crates/fcb-store"
cargo test --test fcb_080_production "$@"
cd "$REPO_ROOT"

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-080-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-080] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-080] PASS (all required routes executed)"
