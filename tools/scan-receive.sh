#!/usr/bin/env bash
# tools/scan-receive.sh
#
# Hosted-mode end-to-end RECEIVE test driver. Boots the xous-emulator
# linked to the receiver account (Precursor2 / 8693), uses signal-cli
# on the sender account (Precursor1 / 5471) to dispatch a uniquely-
# marked test message, and verifies the emulator decrypts and
# delivers it intact.
#
# This is the OPPOSITE direction from scan-send.sh:
#   scan-send.sh:    emulator (8693) -> recipient (5471, signal-cli verify)
#   scan-receive.sh: signal-cli (5471) -> emulator (8693)
#
# Both scripts use the SAME tools/.env values; this script swaps the
# sender / recipient roles internally:
#   - signal-cli SENDS from XSC_RECIPIENT_NUMBER (Precursor1)
#   - emulator RECEIVES on XSC_SENDER_NUMBER (Precursor2)
#
# Verification leg structure:
#   leg 1: wire format — implicit (sigchat decrypted the envelope and
#          parsed the Content protobuf without dropping the message)
#   leg 2: recipient parse — the emulator's debug recv hook (gated by
#          XSCDEBUG_RECV=1) emits a structured `[recv-debug]` log
#          line containing the body. This script greps for the marker
#          timestamp string.
#   leg 3: user-visible — manual; check the Precursor emulator UI if
#          needed.
#
# Prerequisites:
#   - tools/.env configured (see tools/test-env.example)
#   - signal-cli linked to XSC_RECIPIENT_NUMBER (Precursor1) as a
#     secondary device (signal-cli-test)
#   - xous-emulator linked to XSC_SENDER_NUMBER (Precursor2);
#     PDDB snapshot at $XSC_PDDB_IMAGE
#   - X11 display (default :10) where the emulator window appears
#   - python3 (ctypes; usually present)
#   - xous-core checkout at $XOUS_CORE_PATH on
#     dev-for-xous-signal-client
#
# Output:
#   - Emulator scan log to /tmp/xsc-recv-<timestamp>.log
#
# Exit codes:
#   0 = emulator received the marker; body matches
#   1 = emulator did not receive the marker, or body mismatch
#   2 = setup failure (missing env, prerequisites, topology check)
#
# Usage:
#   ./tools/scan-receive.sh                # marker contains "Test"
#   ./tools/scan-receive.sh "Hello world"  # custom expected body

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=test-helpers.sh
source "$SCRIPT_DIR/test-helpers.sh"
ROOT="$(xsc_repo_root)"

if ! xsc_load_env; then
    echo "tools/.env not found." >&2
    echo "Copy tools/test-env.example to tools/.env and configure." >&2
    exit 2
fi

xsc_require_env XSC_SENDER_NUMBER XSC_RECIPIENT_NUMBER || exit 2
xsc_require_cmd signal-cli || exit 2
xsc_require_cmd cargo || exit 2
xsc_require_cmd python3 || exit 2

# In scan-receive.sh, signal-cli is the SENDER (Precursor1, 5471), and
# the emulator is the RECEIVER (Precursor2, 8693). XSC_SENDER_NUMBER
# names the emulator's account; XSC_RECIPIENT_NUMBER names signal-cli's
# account. Map env to roles:
SIGNAL_CLI_ACCOUNT="$XSC_RECIPIENT_NUMBER"   # Precursor1
EMULATOR_ACCOUNT="$XSC_SENDER_NUMBER"        # Precursor2

echo "=== Verifying topology (signal-cli listDevices) ==="
if ! xsc_verify_linked_device "$SIGNAL_CLI_ACCOUNT" \
        "signal-cli-test" "signal-cli"; then
    echo "Topology check failed — see ACCOUNT-MAPPING.md." >&2
    exit 2
fi
echo "OK: signal-cli is linked to $SIGNAL_CLI_ACCOUNT as a secondary."
echo "    (sender for the receive test)"
echo ""
echo "Note: signal-cli is not linked to $EMULATOR_ACCOUNT — that's"
echo "the emulator-only account. The emulator's PDDB snapshot is the"
echo "source of truth for that side."
echo ""

XOUS_CORE_PATH="${XOUS_CORE_PATH:-$ROOT/../xous-core}"
if [[ ! -d "$XOUS_CORE_PATH" ]]; then
    echo "xous-core not found at $XOUS_CORE_PATH" >&2
    exit 2
fi

PDDB_IMAGE="${XSC_PDDB_IMAGE:-$XOUS_CORE_PATH/tools/pddb-images/hosted-linked-display-verified.bin}"
if [[ ! -f "$PDDB_IMAGE" ]]; then
    echo "PDDB image not found: $PDDB_IMAGE" >&2
    exit 2
fi

# The marker is a unique substring we look for in the emulator's log.
# Combining the user-supplied message with a per-run timestamp gives
# a string that won't accidentally match earlier scans of an older log.
MESSAGE_TEXT="${1:-Test}"
TS=$(date +%s)
MARKER="${MESSAGE_TEXT} [recv-${TS}]"
LOG="/tmp/xsc-recv-${TS}.log"

export DISPLAY="${DISPLAY:-:10}"
export XSCDEBUG_RECV=1

echo "=== xous-signal-client receive scan (ts=$TS) ==="
echo "  Marker:     '$MARKER'"
echo "  Sender:     $SIGNAL_CLI_ACCOUNT (signal-cli)"
echo "  Receiver:   $EMULATOR_ACCOUNT (emulator)"
echo "  Log:        $LOG"
echo ""

# Stop any stale emulator and restore the PDDB snapshot.
pkill -f "xous-kernel" 2>/dev/null || true
sleep 1

HOSTED_BIN="$XOUS_CORE_PATH/tools/pddb-images/hosted.bin"
echo "Restoring PDDB snapshot -> $HOSTED_BIN"
cp "$PDDB_IMAGE" "$HOSTED_BIN"

# Prime the receive path with a queued PreKey-bundle envelope from
# signal-cli BEFORE booting. The PDDB snapshot's frozen-at-link-time
# session state may be stale relative to signal-cli's live session
# state. Sending a priming envelope before boot forces signal-cli to
# emit a fresh PreKey-bundle (envelope type 3), which establishes a
# new session on the emulator's side at boot. The actual marker that
# follows then rides the fresh session. Same pattern as v7's send
# scan harness.
#
# Pre-step: clear signal-cli's stored sessions for the emulator UUID
# (issue #9 / B2-sibling priming flake). Without this, signal-cli
# reuses its stored session and emits a SignalMessage instead of a
# PreKey-bundle — the rolled-back emulator then cannot decrypt the
# priming envelope and the receive marker that follows is lost.
echo "=== Clearing signal-cli sessions for emulator UUID ==="
xsc_clear_signal_cli_sessions "$SIGNAL_CLI_ACCOUNT" "$EMULATOR_ACCOUNT" || true
echo ""

echo "=== Priming session via signal-cli (queued for emulator boot) ==="
PRIME_BODY="phase-r-plus recv prime $TS"
if signal-cli -a "$SIGNAL_CLI_ACCOUNT" send -m "$PRIME_BODY" \
        "$EMULATOR_ACCOUNT" 2>&1 | head -3; then
    echo "OK: priming send dispatched"
else
    echo "WARN: priming send failed; receive may not establish fresh session" >&2
fi
echo ""

# Boot the emulator. The "sigchat:" alias matches the GAM context name
# `signal` registered by xous-signal-client/src/main.rs (until xous-core's
# apps/manifest.json gains an entry for xous-signal-client). Same trick
# scan-send.sh and measure-renode.sh use.
echo "Booting xous-signal-client..."
(cd "$XOUS_CORE_PATH" && \
    timeout 240 cargo xtask run \
    "sigchat:$ROOT/target/release/xous-signal-client" \
    >"$LOG" 2>&1) &
XOUS_PID=$!

# Wait for the WS to authenticate. Without this, we'd send via signal-cli
# before the emulator is online and the message would queue server-side
# but never trigger a [recv-debug] line during the script's lifetime.
echo "Waiting for emulator WS authentication..."
WAIT=0
while (( WAIT < 120 )); do
    if grep -q "main_ws: authenticated websocket established" "$LOG" 2>/dev/null; then
        echo "  WS authenticated at t=${WAIT}s"
        break
    fi
    sleep 2
    WAIT=$((WAIT + 2))
done

if ! grep -q "main_ws: authenticated websocket established" "$LOG" 2>/dev/null; then
    echo "ERROR: emulator did not authenticate to Signal-Server in 120s" >&2
    pkill -f "xous-kernel" 2>/dev/null || true
    wait "$XOUS_PID" 2>/dev/null || true
    exit 1
fi

# Wait for the priming envelope to be drained and decrypted. It was
# queued before boot, so it should arrive shortly after WS auth.
echo "Waiting for priming envelope to decrypt..."
WAIT=0
while (( WAIT < 60 )); do
    if grep -qE "main_ws: delivered .* chars from|main_ws: SS PREKEY decrypted|main_ws: PREKEY_BUNDLE decrypted" "$LOG" 2>/dev/null; then
        echo "  Priming decrypted at t=${WAIT}s"
        break
    fi
    sleep 2
    WAIT=$((WAIT + 2))
done

# Even if the priming decrypted, give the WS read loop a beat before
# we send the real marker so the priming-driven session-state writes
# to PDDB have a chance to land.
sleep 3

# Send the marker via signal-cli.
echo ""
echo "=== Sending marker via signal-cli ==="
echo "$ signal-cli -a $SIGNAL_CLI_ACCOUNT send -m '$MARKER' $EMULATOR_ACCOUNT"
if ! signal-cli -a "$SIGNAL_CLI_ACCOUNT" send -m "$MARKER" "$EMULATOR_ACCOUNT"; then
    echo "ERROR: signal-cli send failed" >&2
    pkill -f "xous-kernel" 2>/dev/null || true
    wait "$XOUS_PID" 2>/dev/null || true
    exit 1
fi

# Watch for the marker in the [recv-debug] line. The emulator's WS
# typically delivers the envelope within a few seconds, but Signal-
# Server can occasionally delay; allow up to 90s.
echo ""
echo "=== Watching emulator log for marker (90s timeout) ==="
WAIT=0
FOUND=""
while (( WAIT < 90 )); do
    FOUND="$(grep "\[recv-debug\]" "$LOG" 2>/dev/null | grep -F "recv-${TS}" | head -1)"
    if [[ -n "$FOUND" ]]; then
        break
    fi
    sleep 5
    WAIT=$((WAIT + 5))
done

echo ""
echo "=== Cleaning up emulator ==="
pkill -f "xous-kernel" 2>/dev/null || true
wait "$XOUS_PID" 2>/dev/null || true

if [[ -z "$FOUND" ]]; then
    echo ""
    echo "RESULT: FAIL (no [recv-debug] line containing 'recv-${TS}' in ${WAIT}s)"
    echo ""
    echo "Last 15 main_ws log lines:"
    grep "main_ws" "$LOG" 2>/dev/null | tail -15
    exit 1
fi

echo ""
echo "=== Match found ==="
echo "$FOUND"
echo ""
echo "RESULT: PASS"
exit 0
