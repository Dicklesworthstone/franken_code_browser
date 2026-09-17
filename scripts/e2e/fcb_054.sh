#!/bin/bash
# FCB-054.A production verification scenario:
# Full virtual semantic accessibility surfaces and keyboard navigation.
# Verifies:
# - Virtual outline tree bounded window retrieval (MAX_ACCESSIBILITY_WINDOW, no million-node trees).
# - Hierarchical parent/child/sibling navigation without spatial vision.
# - Virtual search results with linear navigation and active selection.
# - Document semantic structure: structural milestone jumps (headings, tables, links, lines) and linear reading stream.
# - City mode non-spatial accessibility: translates 3D building height/size metrics into descriptive text.
# - Keyboard navigation engine: full pointer equivalence, visible focus independent of selection, and return focus.
# - Negative controls detecting window exhaustion, focus conflation, and missing non-spatial metrics.
#
# Supported routes:
#   scripts/e2e/fcb_054.sh [cargo-test-args...]
#   scripts/e2e/fcb_054.sh --lane runtime
#   scripts/e2e/fcb_054.sh --lane production
#   scripts/e2e/fcb_054.sh --lane all
#
# Environment:
#   FCB_054_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-054-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-054/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_054_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_054_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-054/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-054-receipts-$RUN_ID"
mkdir -p "$RECEIPTS_DIR"
export FCB_RECEIPTS_DIR="$RECEIPTS_DIR"

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

echo "[fcb-054] route: headless cargo test (fcb-runtime lib, fcb_054_virtual_accessibility)"
echo "[fcb-054] lane: $LANE"
echo "[fcb-054] run id: $RUN_ID"

case "$LANE" in
    runtime)
        echo "[fcb-054] running fcb-runtime unit tests..."
        cargo test -p fcb-runtime --lib "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    production)
        echo "[fcb-054] running fcb_054_virtual_accessibility tests..."
        cargo test -p fcb-runtime --test fcb_054_virtual_accessibility "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    all)
        echo "[fcb-054] running all FCB-054 test suites..."
        cargo test -p fcb-runtime --lib "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test -p fcb-runtime --test fcb_054_virtual_accessibility "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    *)
        echo "[fcb-054] unknown lane: $LANE" >&2
        exit 1
        ;;
esac

# Retain the bounded redacted receipts produced by this run.
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" | wc -l | tr -d ' ')
    echo "[fcb-054] receipts archived ($RECEIPT_COUNT receipts): $ARTIFACT_DIR/receipts"
    if [ "$LANE" = "production" ] || [ "$LANE" = "all" ]; then
        if [ "$RECEIPT_COUNT" -lt 6 ]; then
            echo "[fcb-054] ERROR: expected at least 6 receipts, got $RECEIPT_COUNT" >&2
            exit 1
        fi
    fi
fi

# Write structured outcome report.
cat <<EOF > "$ARTIFACT_DIR/summary.json"
{
  "scenario": "FCB-054.A full virtual semantic accessibility surfaces and keyboard navigation",
  "lane": "$LANE",
  "run_id": "$RUN_ID",
  "receipt_count": ${RECEIPT_COUNT:-0},
  "receipts_dir": "$ARTIFACT_DIR/receipts",
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "exit_code": 0
}
EOF

echo "[fcb-054] verification succeeded: summary written to $ARTIFACT_DIR/summary.json"
