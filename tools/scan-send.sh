#!/usr/bin/env bash
# tools/scan-send.sh
#
# End-to-end send test for xous-signal-client. Boots Xous from a
# linked-account PDDB snapshot, navigates the emulator UI to the
# xous-signal-client app, types the test message, and watches the
# scan log for the send result. Wire bytes are captured via
# XSCDEBUG_DUMP for offline verification by tools/decode-wire.sh.
#
# This is family 2 of the testing methodology — hosted-mode E2E.
# It requires real Signal accounts and live network. It cannot run
# in CI.
#
# Prerequisites:
#   - tools/.env configured (see tools/test-env.example)
#   - signal-cli installed and on PATH; linked as a secondary device
#     on the recipient account
#   - xous-signal-client linked as a secondary device on the sender
#     account; PDDB snapshot at $XSC_PDDB_IMAGE
#   - X11 display (default :10) where the emulator window appears
#   - python3 with X11 bindings (ctypes; usually present)
#   - xous-core checkout at $XOUS_CORE_PATH (default ../xous-core) on
#     branch feat/05-curve25519-dalek-4.1.3
#
# Output:
#   - Wire bytes to /tmp/xsc-wire-dump.txt (XSCDEBUG_DUMP=1)
#   - Scan log to /tmp/xsc-scan-<timestamp>.log
#
# Exit codes:
#   0 = post: sent observed in log
#   1 = send failed (RetryExhausted, send error, or no post: sent)
#   2 = setup failure (missing env, prerequisites, or build error)
#
# Usage:
#   ./tools/scan-send.sh                # send "Test"
#   ./tools/scan-send.sh "Hello world"  # send custom text

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
xsc_require_cmd signal-cli "https://github.com/AsamK/signal-cli/releases" || exit 2
xsc_require_cmd cargo || exit 2
xsc_require_cmd python3 || exit 2

# Topology pre-check (canonical mapping in ~/workdir/ACCOUNT-MAPPING.md).
# In the send test, signal-cli is the recipient-side verifier. It must
# be linked as `signal-cli-test` to XSC_RECIPIENT_NUMBER (Precursor1).
# Refuse to send if the link isn't there — guards against the topology
# confusion that affected several earlier sessions.
echo "=== Verifying topology (signal-cli listDevices) ==="
if ! xsc_verify_linked_device "$XSC_RECIPIENT_NUMBER" \
        "signal-cli-test" "signal-cli"; then
    echo "Topology check failed — see ACCOUNT-MAPPING.md." >&2
    exit 2
fi
echo "OK: signal-cli is linked to $XSC_RECIPIENT_NUMBER as a secondary."
echo ""

XOUS_CORE_PATH="${XOUS_CORE_PATH:-$ROOT/../xous-core}"
if [[ ! -d "$XOUS_CORE_PATH" ]]; then
    echo "xous-core not found at $XOUS_CORE_PATH" >&2
    exit 2
fi

PDDB_IMAGE="${XSC_PDDB_IMAGE:-$XOUS_CORE_PATH/tools/pddb-images/hosted-linked-display-verified.bin}"
if [[ ! -f "$PDDB_IMAGE" ]]; then
    echo "PDDB image not found: $PDDB_IMAGE" >&2
    echo "Set XSC_PDDB_IMAGE in tools/.env to a linked-account snapshot." >&2
    exit 2
fi

MESSAGE="${1:-Test}"
TS=$(date +%s)
LOG="/tmp/xsc-scan-${TS}.log"
WIRE_DUMP="/tmp/xsc-wire-dump.txt"

export DISPLAY="${DISPLAY:-:10}"
export XSCDEBUG_DUMP=1

echo "=== xous-signal-client send scan (ts=$TS) ==="
echo "  Message:    '$MESSAGE'"
echo "  Sender:     $XSC_SENDER_NUMBER (linked xous-signal-client)"
echo "  Recipient:  $XSC_RECIPIENT_NUMBER (signal-cli on this machine)"
echo "  Log:        $LOG"
echo "  Wire dump:  $WIRE_DUMP"
echo ""

# Clear any stale wire dump so we know the next one is from this run.
: >"$WIRE_DUMP"

# Prime the emulator's outgoing recipient by sending a queued inbound
# from signal-cli. The hosted PDDB snapshots used in this project are
# captured at a clean linked-account state with no `default.peer` key;
# without a peer to address, SigChat::post falls through to local-echo.
# An inbound message decrypts on emulator boot and triggers
# set_current_recipient, populating default.peer with signal-cli's
# UUID. The emulator then has somewhere to send our subsequent
# typed message back.
#
# Pre-step: clear signal-cli's stored sessions for the emulator UUID
# (issue #9 / B2-sibling priming flake). Without this, signal-cli may
# reuse a stale session that advanced past the PDDB snapshot's frozen
# state and send a SignalMessage instead of a PreKey-bundle, which
# the rolled-back emulator cannot decrypt.
echo "=== Clearing signal-cli sessions for emulator UUID ==="
xsc_clear_signal_cli_sessions "$XSC_RECIPIENT_NUMBER" "$XSC_SENDER_NUMBER" || true
echo ""

echo "=== Priming default.peer via signal-cli ==="
PRIME_BODY="phase-r-plus prime $TS"
if signal-cli -a "$XSC_RECIPIENT_NUMBER" send -m "$PRIME_BODY" \
        "$XSC_SENDER_NUMBER" 2>&1 | head -3; then
    echo "OK: priming send dispatched"
else
    echo "WARN: priming send failed; emulator may local-echo only" >&2
fi
echo ""

# Kill any stale Xous emulator.
pkill -f "xous-kernel" 2>/dev/null || true
sleep 1

# Restore the linked PDDB snapshot to the live hosted.bin path. Sigchat
# / xous-signal-client use this as the persistent backing for their
# linked account state (sessions, prekeys, identity, etc.). Restoring a
# known-good snapshot gives each scan a deterministic starting point.
HOSTED_BIN="$XOUS_CORE_PATH/tools/pddb-images/hosted.bin"
echo "Restoring PDDB snapshot -> $HOSTED_BIN"
cp "$PDDB_IMAGE" "$HOSTED_BIN"

# Boot xous-signal-client via xous-core's xtask. The "sigchat:" alias
# resolves the GAM context name to "signal" — matching what
# xous-signal-client/src/main.rs registers — while the binary path
# points at the freshly built xous-signal-client release ELF.
echo "Booting xous-signal-client..."
(cd "$XOUS_CORE_PATH" && \
    timeout 360 cargo xtask run \
    "sigchat:$ROOT/target/release/xous-signal-client" \
    >"$LOG" 2>&1) &
XOUS_PID=$!

# Wait for system ready signals.
echo "Waiting for system to settle..."
WAIT=0
while (( WAIT < 120 )); do
    if grep -q "my PID is" "$LOG" 2>/dev/null && \
       grep -q "xous_signal_client\|SigChat" "$LOG" 2>/dev/null; then
        echo "  System up at t=${WAIT}s"
        break
    fi
    sleep 2
    WAIT=$((WAIT + 2))
done
sleep 10

# Drive navigation and typing via X11 events. Captures the Precursor
# emulator window by name, presses Home/Down to navigate to the app,
# waits 25s for the WS receive worker to pull the priming inbound and
# populate default.peer, then types the message and presses Enter.
echo ""
echo "=== Driving emulator (Home/Down navigation + type + Enter) ==="
python3 - "$MESSAGE" "$DISPLAY" <<'PYEOF'
import ctypes, time, os, sys

MESSAGE = sys.argv[1]
DISPLAY = sys.argv[2]

X11 = ctypes.cdll.LoadLibrary("libX11.so.6")
c_ulong = ctypes.c_ulong; c_int = ctypes.c_int; c_uint = ctypes.c_uint

X11.XOpenDisplay.restype = ctypes.c_void_p
X11.XOpenDisplay.argtypes = [ctypes.c_char_p]
X11.XSync.argtypes = [ctypes.c_void_p, c_int]
X11.XDefaultRootWindow.restype = c_ulong
X11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
X11.XKeysymToKeycode.restype = c_uint
X11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, c_ulong]
X11.XFlush.argtypes = [ctypes.c_void_p]
X11.XFetchName.restype = c_int
X11.XFetchName.argtypes = [ctypes.c_void_p, c_ulong, ctypes.POINTER(ctypes.c_char_p)]
X11.XQueryTree.restype = c_int
X11.XQueryTree.argtypes = [ctypes.c_void_p, c_ulong, ctypes.POINTER(c_ulong),
    ctypes.POINTER(c_ulong), ctypes.POINTER(ctypes.POINTER(c_ulong)), ctypes.POINTER(c_uint)]
X11.XFree.argtypes = [ctypes.c_void_p]
X11.XSendEvent.restype = c_int
X11.XSendEvent.argtypes = [ctypes.c_void_p, c_ulong, c_int, c_ulong, ctypes.c_void_p]

class XEvent(ctypes.Union):
    class XKeyEvent(ctypes.Structure):
        _fields_ = [
            ('type', c_int), ('serial', c_ulong), ('send_event', c_int),
            ('display', ctypes.c_void_p), ('window', c_ulong), ('root', c_ulong),
            ('subwindow', c_ulong), ('time', c_ulong),
            ('x', c_int), ('y', c_int), ('x_root', c_int), ('y_root', c_int),
            ('state', c_uint), ('keycode', c_uint), ('same_screen', c_int),
        ]
    _fields_ = [('key', XKeyEvent), ('pad', ctypes.c_char * 192)]

def find_win(dpy, root, name_b):
    cname = ctypes.c_char_p()
    X11.XFetchName(dpy, root, ctypes.byref(cname))
    if cname.value and name_b in cname.value.lower():
        return root
    r=c_ulong(); p=c_ulong(); ch=ctypes.POINTER(c_ulong)(); n=c_uint()
    if X11.XQueryTree(dpy, root, ctypes.byref(r), ctypes.byref(p), ctypes.byref(ch), ctypes.byref(n)):
        children = [ch[i] for i in range(n.value)]
        if n.value: X11.XFree(ch)
        for c in children:
            w = find_win(dpy, c, name_b)
            if w: return w
    return None

def press(dpy, win, root, kc, wait, label, shift=False):
    ev = XEvent()
    ev.key.type = 2
    ev.key.send_event = 1
    ev.key.display = dpy
    ev.key.window = win
    ev.key.root = root
    ev.key.subwindow = 0
    ev.key.time = 0
    ev.key.x = ev.key.y = ev.key.x_root = ev.key.y_root = 0
    ev.key.state = 1 if shift else 0
    ev.key.keycode = kc
    ev.key.same_screen = 1
    X11.XSendEvent(dpy, win, 1, 1, ctypes.byref(ev))
    X11.XFlush(dpy)
    time.sleep(0.05)
    ev.key.type = 3
    X11.XSendEvent(dpy, win, 1, 2, ctypes.byref(ev))
    X11.XFlush(dpy)
    print(f"  [{label}] kc={kc}, wait={wait}s, shift={shift}")
    sys.stdout.flush()
    time.sleep(wait)

dpy = X11.XOpenDisplay(DISPLAY.encode())
if not dpy:
    print("ERROR: cannot open display", file=sys.stderr); sys.exit(1)
root = X11.XDefaultRootWindow(dpy)

win = None
for attempt in range(30):
    win = find_win(dpy, root, b"precursor")
    if win:
        break
    print(f"  waiting for Precursor window (attempt {attempt+1})...")
    sys.stdout.flush()
    time.sleep(2)
if not win:
    print("ERROR: Precursor window not found", file=sys.stderr); sys.exit(1)

kc_home = X11.XKeysymToKeycode(dpy, 0xFF50)
kc_down = X11.XKeysymToKeycode(dpy, 0xFF54)
kc_return = X11.XKeysymToKeycode(dpy, 0xFF0D)

print("Navigating to xous-signal-client...")
press(dpy, win, root, kc_home, 1.5,  "1. Home -> open main menu")
press(dpy, win, root, kc_down, 0.3,  "2. Down -> App")
press(dpy, win, root, kc_home, 4.5,  "3. Home -> select App")
press(dpy, win, root, kc_down, 0.3,  "4. Down -> signal")
press(dpy, win, root, kc_home, 25.0, "5. Home -> open xous-signal-client (wait 25s for WS pull + decrypt)")

print(f"Typing message: '{MESSAGE}'")
for ch in MESSAGE:
    if ch == '!':
        kc = X11.XKeysymToKeycode(dpy, ord('1'))
        press(dpy, win, root, kc, 0.1, "char '!' (shift+1)", shift=True)
    elif ch == ' ':
        kc = X11.XKeysymToKeycode(dpy, 0x0020)
        press(dpy, win, root, kc, 0.1, "char ' '")
    elif ch.isupper():
        kc = X11.XKeysymToKeycode(dpy, ord(ch.lower()))
        press(dpy, win, root, kc, 0.1, f"char '{ch}' (shift+{ch.lower()})", shift=True)
    else:
        kc = X11.XKeysymToKeycode(dpy, ord(ch))
        press(dpy, win, root, kc, 0.1, f"char '{ch}'")

print("Pressing Enter to submit")
press(dpy, win, root, kc_return, 2.0, "Enter -> submit")
PYEOF

echo ""
echo "=== Watching scan log for send completion (90s timeout) ==="
WAIT=0
RESULT="timeout"
while (( WAIT < 90 )); do
    if grep -q "post: sent to" "$LOG" 2>/dev/null; then
        RESULT="sent"
        break
    fi
    if grep -qE "post: send failed|RetryExh" "$LOG" 2>/dev/null; then
        RESULT="failed"
        break
    fi
    sleep 5
    WAIT=$((WAIT + 5))
done

echo ""
echo "=== Send result ($RESULT after ${WAIT}s) ==="
grep -E "got SigchatOp::Post|post:|send:|attempt|sent to|RetryExh" "$LOG" 2>/dev/null | tail -15

echo ""
echo "=== Cleaning up emulator ==="
pkill -f "xous-kernel" 2>/dev/null || true
wait "$XOUS_PID" 2>/dev/null || true

case "$RESULT" in
    sent) ;;
    failed)
        echo ""
        echo "RESULT: FAIL (send failed in log)"
        exit 1 ;;
    *)
        echo ""
        echo "RESULT: FAIL (no terminal log line in 90s)"
        exit 1 ;;
esac

# leg-1 confirmed: post: sent observed.
echo ""
echo "=== leg-1 PASS: post: sent observed ==="
echo "  Wire dump: $WIRE_DUMP"

# leg-2: recipient parse — run signal-cli receive on the recipient account
# and confirm the body arrived at the protocol layer. Give Signal-Server
# a moment to deliver the envelope to signal-cli's device before polling.
echo ""
echo "=== leg-2: recipient parse via signal-cli receive (waiting 8s) ==="
sleep 8
RECV_OUT=$(signal-cli -a "$XSC_RECIPIENT_NUMBER" receive 2>&1 || true)
echo "$RECV_OUT" | head -30

if echo "$RECV_OUT" | grep -qF "Body: $MESSAGE"; then
    echo ""
    echo "=== leg-2 PASS: Body: $MESSAGE confirmed by signal-cli ==="
    echo ""
    echo "RESULT: PASS (leg-1 + leg-2)"
    echo "  Run ./tools/decode-wire.sh to verify wire bytes."
    echo "  Check both phones to confirm leg-3 (user-visible)."
    exit 0
elif echo "$RECV_OUT" | grep -qiE "InvalidMessageException.*decryption failed|ProtocolInvalidMessageException"; then
    # B2 (issue #8) — closed 2026-04-28 after the receive-direction
    # priming-flake sibling was fixed in PR #30 (issue #9). Three
    # consecutive scan-send PASSes confirmed B2 send-direction is no
    # longer reachable. We keep this branch as a *regression detector*
    # so a future re-occurrence surfaces with a clear pointer rather
    # than a generic "no Body line" FAIL — but exit 1 (FAIL), not 87
    # (KNOWN_FAIL). bug-arcs/b005 is the historical record.
    echo ""
    echo "=== leg-2 FAIL: signal-cli libsignal decrypt failure (B2 regression?) ==="
    echo "  Pattern matches issue #8 (closed 2026-04-28). If reproducible:"
    echo "  - Verify session state on both ends matches (pre-test session-clear)"
    echo "  - Re-open issue #8 with reproduction steps"
    echo "  - See bug-arcs/b005 for historical investigation notes"
    echo ""
    echo "RESULT: FAIL (leg-1 PASS; leg-2 FAIL — possible B2 regression)"
    exit 1
else
    echo ""
    echo "=== leg-2 FAIL: no Body: line and no known-exception pattern ==="
    echo "  Full signal-cli output above. Run with --verbose for envelope detail."
    echo ""
    echo "RESULT: FAIL (leg-1 PASS; leg-2 FAIL — unexpected receive output)"
    exit 1
fi
