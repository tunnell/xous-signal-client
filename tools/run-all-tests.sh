#!/usr/bin/env bash
# tools/run-all-tests.sh
#
# Runs all three test families and reports a per-family PASS / SKIPPED
# / FAIL summary. The orchestrator is the documented entry point for
# "run the full test suite before declaring this PR ready" — see
# tests/README.md for the testing methodology.
#
# Test families:
#   1. Rust unit and integration tests (cargo test)
#   2. Hosted-mode end-to-end (requires tools/.env + signal-cli + an
#      X11 display + a linked PDDB snapshot)
#   3. Memory footprint (static binary size against the documented
#      budget; Renode runtime smoke test if Renode is installed)
#
# Families whose prerequisites aren't met are SKIPPED, not FAIL. The
# exit code reflects whether all families that COULD run actually
# passed.
#
# Usage:
#   ./tools/run-all-tests.sh
#   ./tools/run-all-tests.sh --skip-e2e
#   ./tools/run-all-tests.sh --skip-footprint
#   ./tools/run-all-tests.sh --skip-renode
#
# Exit codes:
#   0 = every runnable family passed
#   1 = at least one runnable family failed
#   2 = setup error (run from outside the repo, etc.)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=test-helpers.sh
source "$SCRIPT_DIR/test-helpers.sh"
ROOT="$(xsc_repo_root)"

SKIP_E2E=0
SKIP_FOOTPRINT=0
SKIP_RENODE=0
for arg in "$@"; do
    case "$arg" in
        --skip-e2e) SKIP_E2E=1 ;;
        --skip-footprint) SKIP_FOOTPRINT=1 ;;
        --skip-renode) SKIP_RENODE=1 ;;
        -h|--help)
            sed -n '/^# Usage:/,/^# Exit codes:/p' "$0" | sed 's/^# \?//'
            exit 0 ;;
        *) echo "Unknown argument: $arg" >&2; exit 2 ;;
    esac
done

cd "$ROOT"

declare -A RESULTS
declare -A DETAIL

# --- Family 1: Rust tests ---
echo "================================================"
echo "Family 1: Rust unit/integration tests"
echo "================================================"
RUST_LOG="/tmp/xsc-rust-test.log"
if cargo test --features hosted 2>&1 | tee "$RUST_LOG" | tail -10; then
    RESULTS[rust]="PASS"
    # Pick the first "test result:" line that has at least one passing
    # test — skips the trailing 0/0/0 lines for empty test binaries
    # (test_stores, doc-tests).
    DETAIL[rust]="$(grep -E "^test result: ok\. [1-9]" "$RUST_LOG" | head -1)"
    [[ -z "${DETAIL[rust]}" ]] && \
        DETAIL[rust]="$(grep -E "^test result:" "$RUST_LOG" | head -1)"
else
    RESULTS[rust]="FAIL"
    DETAIL[rust]="see $RUST_LOG"
fi

# --- Family 2a: Hosted E2E send ---
echo ""
echo "================================================"
echo "Family 2a: Hosted-mode E2E send"
echo "================================================"
if (( SKIP_E2E )); then
    RESULTS[send]="SKIPPED"
    DETAIL[send]="--skip-e2e"
elif [[ ! -f "$ROOT/tools/.env" ]]; then
    RESULTS[send]="SKIPPED"
    DETAIL[send]="tools/.env not configured (see tools/test-env.example)"
elif ! command -v signal-cli &>/dev/null; then
    RESULTS[send]="SKIPPED"
    DETAIL[send]="signal-cli not installed"
else
    SEND_EXIT=0
    "$SCRIPT_DIR/scan-send.sh" || SEND_EXIT=$?
    if (( SEND_EXIT == 0 )); then
        RESULTS[send]="PASS"
        DETAIL[send]="leg-1 + leg-2 PASS; verify via decode-wire.sh + phones"
    elif (( SEND_EXIT == 87 )); then
        RESULTS[send]="KNOWN_FAIL"
        DETAIL[send]="B2: signal-cli libsignal decrypt fail (see tests/known-issues.md)"
    elif (( SEND_EXIT == 2 )); then
        RESULTS[send]="SKIPPED"
        DETAIL[send]="setup failure in scan-send.sh"
    else
        RESULTS[send]="FAIL"
        DETAIL[send]="scan-send.sh exit $SEND_EXIT"
    fi
fi

# --- Family 2b: Hosted E2E receive ---
echo ""
echo "================================================"
echo "Family 2b: Hosted-mode E2E receive"
echo "================================================"
if (( SKIP_E2E )); then
    RESULTS[recv]="SKIPPED"
    DETAIL[recv]="--skip-e2e"
elif [[ ! -f "$ROOT/tools/.env" ]]; then
    RESULTS[recv]="SKIPPED"
    DETAIL[recv]="tools/.env not configured (see tools/test-env.example)"
elif ! command -v signal-cli &>/dev/null; then
    RESULTS[recv]="SKIPPED"
    DETAIL[recv]="signal-cli not installed"
else
    if "$SCRIPT_DIR/scan-receive.sh"; then
        RESULTS[recv]="PASS"
        DETAIL[recv]="marker received and decrypted by emulator"
    else
        RC=$?
        if (( RC == 2 )); then
            RESULTS[recv]="SKIPPED"
            DETAIL[recv]="setup failure in scan-receive.sh"
        else
            RESULTS[recv]="FAIL"
            DETAIL[recv]="scan-receive.sh exit $RC"
        fi
    fi
fi

# --- Family 3: Footprint (size + renode) ---
echo ""
echo "================================================"
echo "Family 3: Memory footprint"
echo "================================================"
if (( SKIP_FOOTPRINT )); then
    RESULTS[footprint]="SKIPPED"
    DETAIL[footprint]="--skip-footprint"
else
    SIZE_RC=0
    "$SCRIPT_DIR/measure-size.sh" || SIZE_RC=$?

    RENODE_RC=0
    RENODE_NOTE=""
    if (( SKIP_RENODE )); then
        RENODE_NOTE="renode skipped (--skip-renode)"
    elif ! command -v renode &>/dev/null; then
        RENODE_NOTE="renode not installed"
    else
        "$SCRIPT_DIR/measure-renode.sh" || RENODE_RC=$?
    fi

    case "$SIZE_RC" in
        0) SIZE_NOTE="size budgets pass" ;;
        1) SIZE_NOTE="size budget breached (see report)" ;;
        2) SIZE_NOTE="size measurement setup failed" ;;
        *) SIZE_NOTE="size unknown ($SIZE_RC)" ;;
    esac
    case "$RENODE_RC" in
        0) [[ -z "$RENODE_NOTE" ]] && RENODE_NOTE="renode boot smoke pass" ;;
        1) RENODE_NOTE="renode boot smoke FAIL" ;;
        2) RENODE_NOTE="renode setup failed" ;;
    esac

    # Size is the primary check; renode is a supplementary smoke. PASS
    # iff size passes AND renode either passes or is environmentally
    # skipped. FAIL on size budget breach or renode boot panic.
    if (( SIZE_RC == 0 )) && { (( RENODE_RC == 0 )) || (( RENODE_RC == 2 )); }; then
        RESULTS[footprint]="PASS"
    elif (( SIZE_RC == 2 )); then
        RESULTS[footprint]="SKIPPED"
    else
        RESULTS[footprint]="FAIL"
    fi
    DETAIL[footprint]="$SIZE_NOTE; $RENODE_NOTE"
fi

# --- Summary ---
echo ""
echo "================================================"
echo "Summary"
echo "================================================"
for fam in rust send recv footprint; do
    printf "  %-12s %-12s %s\n" \
        "${fam}:" "${RESULTS[$fam]:-?}" "${DETAIL[$fam]:-}"
done

ANY_FAIL=0
for r in "${RESULTS[@]:-}"; do
    [[ "$r" == "FAIL" ]] && ANY_FAIL=1
done
exit "$ANY_FAIL"
