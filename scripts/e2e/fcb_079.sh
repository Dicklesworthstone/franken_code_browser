#!/bin/bash
# FCB-079.V production verification scenario:
# Two-instance native embedding and independent host/session shutdown tests.
# Verifies two independent browser sessions embedded in a native host:
# - Separate ArenaOwnerId namespaces, RootId grants, and typed HostDeviceTokens.
# - Shared immutable source provider with per-instance audit accounting.
# - Shared font domain with explicit per-instance consent and privacy barrier.
# - Cross-owner handle oracle: cross-owner captures, views, devices, and state are strictly rejected.
# - Independent lifecycle: view detach and session close during concurrent work (in-flight query,
#   GPU terminal upload, and persistence transactions) cleanly drain without affecting the peer
#   or causing global resource teardown.
# - Negative controls demonstrating oracle detection of duplicate close, detached access, and foreign owners.
#
# Supported routes:
#   scripts/e2e/fcb_079.sh [cargo-test-args...]
#   scripts/e2e/fcb_079.sh --lane production
#   scripts/e2e/fcb_079.sh --lane isolation
#   scripts/e2e/fcb_079.sh --lane concurrent
#   scripts/e2e/fcb_079.sh --lane all
#
# Environment:
#   FCB_079_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-079-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-079/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_079_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_079_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-079/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-079-receipts-$RUN_ID"
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

echo "[fcb-079] route: headless cargo test (two_instance_isolation, concurrent_detach_work, fcb_079_production)"
echo "[fcb-079] lane: $LANE"
echo "[fcb-079] run id: $RUN_ID"

case "$LANE" in
    isolation)
        echo "[fcb-079] running two_instance_isolation test..."
        cargo test --manifest-path examples/fcb-two-instance-host/Cargo.toml \
            --test two_instance_isolation "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    concurrent)
        echo "[fcb-079] running concurrent_detach_work test..."
        cargo test --manifest-path examples/fcb-two-instance-host/Cargo.toml \
            --test concurrent_detach_work "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    production)
        echo "[fcb-079] running fcb_079_production tests..."
        cargo test --manifest-path examples/fcb-two-instance-host/Cargo.toml \
            --test fcb_079_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    all)
        echo "[fcb-079] running all FCB-079 test suites..."
        cargo test --manifest-path examples/fcb-two-instance-host/Cargo.toml \
            --test two_instance_isolation "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test --manifest-path examples/fcb-two-instance-host/Cargo.toml \
            --test concurrent_detach_work "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test --manifest-path examples/fcb-two-instance-host/Cargo.toml \
            --test fcb_079_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    *)
        echo "[fcb-079] unknown lane: $LANE" >&2
        exit 1
        ;;
esac

# Retain the bounded redacted receipts produced by this run.
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" | wc -l | tr -d ' ')
    echo "[fcb-079] receipts archived ($RECEIPT_COUNT receipts): $ARTIFACT_DIR/receipts"
    if [ "$LANE" = "production" ]; then
        if [ "$RECEIPT_COUNT" -lt 5 ]; then
            echo "[fcb-079] ERROR: expected at least 5 receipts, got $RECEIPT_COUNT" >&2
            exit 1
        fi
    elif [ "$LANE" = "all" ]; then
        if [ "$RECEIPT_COUNT" -lt 19 ]; then
            echo "[fcb-079] ERROR: expected at least 19 receipts, got $RECEIPT_COUNT" >&2
            exit 1
        fi
    fi
fi

# Write structured outcome report.
cat <<EOF > "$ARTIFACT_DIR/summary.json"
{
  "scenario": "FCB-079.V production verification",
  "lane": "$LANE",
  "run_id": "$RUN_ID",
  "receipt_count": ${RECEIPT_COUNT:-0},
  "receipts_dir": "$ARTIFACT_DIR/receipts",
  "replay_command": "scripts/e2e/fcb_079.sh --lane $LANE",
  "exit_code": 0
}
EOF

echo "[fcb-079] verification succeeded: summary written to $ARTIFACT_DIR/summary.json"
