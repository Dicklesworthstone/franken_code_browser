#!/bin/bash
# FCB-083 verification scenario: Scan epochs, dirty-hint tracking,
# atomic-save continuity, and orphaned annotations (see
# crates/fcb-source/tests/scan_epochs.rs for the required-case registry).
#
# Supported route:
#   scripts/e2e/fcb_083.sh [cargo-test-args...]
#
# Environment:
#   FCB_083_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-083-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-083/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_083_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_083_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-083/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

echo "[fcb-083] route: headless cargo test (fcb-source/scan_epochs)"
echo "[fcb-083] run id: $RUN_ID"

cargo test --manifest-path crates/fcb-source/Cargo.toml \
    --test scan_epochs "$@"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-083-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-083] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-083] PASS (all required routes executed)"
