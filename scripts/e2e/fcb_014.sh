#!/bin/bash
# FCB-014.V production verification scenario: Spatial hierarchy query,
# hierarchical visible-set traversal, LOD admission, and collision-limited labels
# (see crates/fcb-map/tests/lod_traversal.rs and atlas_visibility.rs).
#
# Supported routes:
#   scripts/e2e/fcb_014.sh [cargo-test-args...]
#   scripts/e2e/fcb_014.sh --lane lod
#   scripts/e2e/fcb_014.sh --lane visibility
#   scripts/e2e/fcb_014.sh --lane all
#
# Environment:
#   FCB_014_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-014-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-014/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_014_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_014_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-014/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

LANE="all"
EXTRA_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --lane)
            LANE="$2"
            shift 2
            ;;
        *)
            EXTRA_ARGS+=("$1")
            shift
            ;;
    esac
done

echo "[fcb-014] route: headless cargo test (fcb-map)"
echo "[fcb-014] lane: $LANE"
echo "[fcb-014] run id: $RUN_ID"

if [ "$LANE" = "lod" ] || [ "$LANE" = "all" ]; then
    echo "[fcb-014] running lod_traversal..."
    cargo test --manifest-path crates/fcb-map/Cargo.toml \
        --test lod_traversal "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
fi

if [ "$LANE" = "visibility" ] || [ "$LANE" = "all" ]; then
    echo "[fcb-014] running atlas_visibility..."
    cargo test --manifest-path crates/fcb-map/Cargo.toml \
        --test atlas_visibility "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
fi

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-014-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-014] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-014] PASS (all required routes executed)"
