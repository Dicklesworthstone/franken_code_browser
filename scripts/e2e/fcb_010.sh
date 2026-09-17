#!/bin/bash
# FCB-010.V production verification scenario: Bounded directory discovery and
# ignore matcher driven together (see crates/fcb-source/tests/
# fcb_010_production.rs for the required-case registry).
#
# Supported route (documented invocation):
#   scripts/e2e/fcb_010.sh [cargo-test-args...]
#
# Environment:
#   FCB_010_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-010-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-010/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_010_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_010_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-010/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

echo "[fcb-010] route: headless cargo test (fcb-source/fcb_010_production)"
echo "[fcb-010] run id: $RUN_ID"

cargo test --manifest-path crates/fcb-source/Cargo.toml \
    --test fcb_010_production "$@"

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-010-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-010] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-010] PASS (all required routes executed)"
