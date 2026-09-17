#!/bin/bash
# FCB-025 production scenario: Path search index, stable fuzzy ranking, and refinement
# (see crates/fcb-search/tests/path_navigation.rs and path_refinement_limits.rs).
#
# Supported route (documented invocation):
#   scripts/e2e/fcb_025.sh [cargo-test-args...]
#
# Environment:
#   FCB_025_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-025-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-025/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_025_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_025_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-025/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

echo "[fcb-025] route: headless cargo test (fcb-search/path_navigation + path_refinement_limits)"
echo "[fcb-025] run id: $RUN_ID"

cargo test --manifest-path crates/fcb-search/Cargo.toml \
    --test path_navigation "$@"

cargo test --manifest-path crates/fcb-search/Cargo.toml \
    --test path_refinement_limits "$@"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-025-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-025] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-025] PASS (all required routes executed)"
