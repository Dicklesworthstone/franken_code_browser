#!/bin/bash
# FCB-031.V production verification scenario: consume upstream FMD nested document/flow
# APIs through thin fcb-document integration.
#
# Supported route (documented invocation):
#   scripts/e2e/fcb_031.sh [cargo-test-args...]
#
# Environment:
#   FCB_031_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-031-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-031/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_031_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_031_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-031/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

echo "[fcb-031] route: headless cargo test (fcb-document/fcb_031_production)"
echo "[fcb-031] run id: $RUN_ID"

cargo test --manifest-path crates/fcb-document/Cargo.toml \
    --test fcb_031_production "$@"

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-031-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-031] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-031] PASS (all required routes executed)"
