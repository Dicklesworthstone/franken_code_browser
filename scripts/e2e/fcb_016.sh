#!/bin/bash
# FCB-016.V production verification scenario: FrankenMarkdown-owned shared
# text/font/run contract and native-route interface.
#
# Supported route (documented invocation):
#   scripts/e2e/fcb_016.sh [cargo-test-args...]
#
# Environment:
#   FCB_016_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-016-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-016/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_016_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_016_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-016/$RUN_ID"
mkdir -p "$ARTIFACT_DIR/receipts"
export FCB_RECEIPTS_DIR="$REPO_ROOT/$ARTIFACT_DIR/receipts"

echo "[fcb-016] route: headless cargo test (fcb-document/fcb_016_production)"
echo "[fcb-016] run id: $RUN_ID"

cargo test --manifest-path crates/fcb-document/Cargo.toml \
    --test fcb_016_production "$@"

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-016-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-016] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-016] PASS (all required routes executed)"
