#!/bin/bash
# FCB-089.B production verification scenario:
# Host-target validation and GPU/reference agreement.
# Verifies:
# - Target format, sample count, device token, and color space validation.
# - Exactly-one drawable lease lifecycle (acquire -> encode -> submit -> present).
# - Safe shader ABI record layouts, alignments (16-byte), Little-Endian serialization, and strides.
# - Semantic transparent order, occlusion masking, depth policies, and projection stability.
# - CPU reference linear SDR compositor.
# - Reference oracles detecting gamma corruption, double premultiply, clip violations, depth flips, and target reuse.
#
# Supported routes:
#   scripts/e2e/fcb_089.sh [cargo-test-args...]
#   scripts/e2e/fcb_089.sh --lane production
#   scripts/e2e/fcb_089.sh --lane render
#   scripts/e2e/fcb_089.sh --lane all
#
# Environment:
#   FCB_089_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-089-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-089/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_089_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_089_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-089/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-089-receipts-$RUN_ID"
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

echo "[fcb-089] route: headless cargo test (fcb-render lib, fcb_089_host_target_agreement)"
echo "[fcb-089] lane: $LANE"
echo "[fcb-089] run id: $RUN_ID"

case "$LANE" in
    render)
        echo "[fcb-089] running fcb-render unit tests..."
        cargo test -p fcb-render --lib "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    production)
        echo "[fcb-089] running fcb_089_host_target_agreement tests..."
        cargo test -p fcb-render --test fcb_089_host_target_agreement "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    all)
        echo "[fcb-089] running all FCB-089 test suites..."
        cargo test -p fcb-render --lib "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test -p fcb-render --test fcb_089_host_target_agreement "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    *)
        echo "[fcb-089] unknown lane: $LANE" >&2
        exit 1
        ;;
esac

# Retain the bounded redacted receipts produced by this run.
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" | wc -l | tr -d ' ')
    echo "[fcb-089] receipts archived ($RECEIPT_COUNT receipts): $ARTIFACT_DIR/receipts"
    if [ "$LANE" = "production" ] || [ "$LANE" = "all" ]; then
        if [ "$RECEIPT_COUNT" -lt 6 ]; then
            echo "[fcb-089] ERROR: expected at least 6 receipts, got $RECEIPT_COUNT" >&2
            exit 1
        fi
    fi
fi

# Write structured outcome report.
cat <<EOF > "$ARTIFACT_DIR/summary.json"
{
  "scenario": "FCB-089.B host-target and GPU/reference agreement verification",
  "lane": "$LANE",
  "run_id": "$RUN_ID",
  "receipt_count": ${RECEIPT_COUNT:-0},
  "receipts_dir": "$ARTIFACT_DIR/receipts",
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "exit_code": 0
}
EOF

echo "[fcb-089] verification succeeded: summary written to $ARTIFACT_DIR/summary.json"
