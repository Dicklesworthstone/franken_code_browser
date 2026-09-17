#!/bin/bash
# FCB-013.V production verification scenario: Stable retained partition-tree
# layout, deterministic ordering, local repair and restorable repack
# (see crates/fcb-map/tests/partition_layout.rs and layout_repair.rs).
#
# Supported routes:
#   scripts/e2e/fcb_013.sh [cargo-test-args...]
#   scripts/e2e/fcb_013.sh --lane layout
#   scripts/e2e/fcb_013.sh --lane repair
#
# Environment:
#   FCB_013_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-013-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-013/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_013_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_013_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-013/$RUN_ID"
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

echo "[fcb-013] route: headless cargo test (fcb-map)"
echo "[fcb-013] lane: $LANE"
echo "[fcb-013] run id: $RUN_ID"

if [ "$LANE" = "layout" ] || [ "$LANE" = "all" ]; then
    echo "[fcb-013] running partition_layout..."
    cargo test --manifest-path crates/fcb-map/Cargo.toml \
        --test partition_layout "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
fi

if [ "$LANE" = "repair" ] || [ "$LANE" = "all" ]; then
    echo "[fcb-013] running layout_repair..."
    cargo test --manifest-path crates/fcb-map/Cargo.toml \
        --test layout_repair "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
fi

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-013-receipts-$RUN_ID"
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    echo "[fcb-013] receipts archived: $ARTIFACT_DIR/receipts"
fi

echo "[fcb-013] PASS (all required routes executed)"
