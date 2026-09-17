#!/bin/bash
# FCB-034.V production verification scenario:
# FrankenMarkdown math/diagram display output plus FCB renderer mapping.
# Verifies:
# - Qualified math corpus with exact source anchor preservation and visible retention.
# - Qualified diagram corpus (Mermaid, DOT) with exact source anchor preservation.
# - Hostile markup and script injection containment with HOSTILE_MARKUP code.
# - Work budget and clip stack depth limits.
# - Color pipeline single premultiplication without double alpha.
# - Negative controls on non-finite coordinates and clip underflow.
#
# Supported routes:
#   scripts/e2e/fcb_034.sh [cargo-test-args...]
#   scripts/e2e/fcb_034.sh --lane production
#   scripts/e2e/fcb_034.sh --lane render
#   scripts/e2e/fcb_034.sh --lane all
#
# Environment:
#   FCB_034_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-034-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-034/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_034_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_034_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-034/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-034-receipts-$RUN_ID"
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

echo "[fcb-034] route: headless cargo test (fcb-render lib, fcb_034_production)"
echo "[fcb-034] lane: $LANE"
echo "[fcb-034] run id: $RUN_ID"

case "$LANE" in
    render)
        echo "[fcb-034] running fcb-render unit tests..."
        cargo test -p fcb-render --lib "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    production)
        echo "[fcb-034] running fcb_034_production tests..."
        cargo test -p fcb-render --test fcb_034_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    all)
        echo "[fcb-034] running all FCB-034 test suites..."
        cargo test -p fcb-render --lib "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test -p fcb-render --test fcb_034_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    *)
        echo "[fcb-034] unknown lane: $LANE" >&2
        exit 1
        ;;
esac

# Retain the bounded redacted receipts produced by this run.
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" | wc -l | tr -d ' ')
    echo "[fcb-034] receipts archived ($RECEIPT_COUNT receipts): $ARTIFACT_DIR/receipts"
    if [ "$LANE" = "production" ] || [ "$LANE" = "all" ]; then
        if [ "$RECEIPT_COUNT" -lt 6 ]; then
            echo "[fcb-034] ERROR: expected at least 6 receipts, got $RECEIPT_COUNT" >&2
            exit 1
        fi
    fi
fi

# Write structured outcome report.
cat <<EOF > "$ARTIFACT_DIR/summary.json"
{
  "scenario": "FCB-034.V production verification",
  "lane": "$LANE",
  "run_id": "$RUN_ID",
  "receipt_count": ${RECEIPT_COUNT:-0},
  "receipts_dir": "$ARTIFACT_DIR/receipts",
  "replay_command": "scripts/e2e/fcb_034.sh --lane $LANE",
  "exit_code": 0
}
EOF

echo "[fcb-034] verification succeeded: summary written to $ARTIFACT_DIR/summary.json"
