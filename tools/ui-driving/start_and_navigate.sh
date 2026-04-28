#!/bin/bash
# Start Xous hosted mode, wait for full initialization, then navigate to sigchat Link.
set -e

WORKDIR=/home/tunnell/workdir
XOUS_DIR=$WORKDIR/xous-core
LOG=$WORKDIR/xous-phase20.log
PID_FILE=$WORKDIR/xous-phase20.pid
NAVIGATE_SCRIPT=$WORKDIR/navigate_to_link.py

export DISPLAY=localhost:10.0

echo "=== Starting Xous phase20 ==="

# Kill any stale Xous processes
pkill -f "xous-kernel" 2>/dev/null || true
sleep 1

# Start Xous with 600s timeout
(cd "$XOUS_DIR" && timeout 600 cargo xtask run "sigchat:../sigchat/target/release/sigchat" >"$LOG" 2>&1) &
XOUS_BG_PID=$!
echo $XOUS_BG_PID > "$PID_FILE"
echo "Xous started, bg PID=$XOUS_BG_PID, log=$LOG"

# Wait for SigChat::new returned (system fully ready)
echo "Waiting for Xous readiness..."
WAIT=0
while [ $WAIT -lt 120 ]; do
    if grep -q "SigChat::new returned" "$LOG" 2>/dev/null; then
        echo "System ready (SigChat::new returned detected at t=${WAIT}s)"
        break
    fi
    sleep 2
    WAIT=$((WAIT + 2))
done

if ! grep -q "SigChat::new returned" "$LOG" 2>/dev/null; then
    echo "ERROR: System did not become ready within 120s"
    tail -20 "$LOG"
    exit 1
fi

# Extra settling time
echo "Waiting 8s for full settlement..."
sleep 8

# Verify the Precursor window exists
echo "Checking for Precursor window..."
WIN=$(DISPLAY=localhost:10.0 python3 -c "
import ctypes, sys
X11 = ctypes.cdll.LoadLibrary('libX11.so.6')
c_ulong = ctypes.c_ulong; c_int = ctypes.c_int; c_uint = ctypes.c_uint
X11.XOpenDisplay.restype = ctypes.c_void_p; X11.XOpenDisplay.argtypes = [ctypes.c_char_p]
X11.XDefaultRootWindow.restype = c_ulong; X11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
X11.XFetchName.restype = c_int; X11.XFetchName.argtypes = [ctypes.c_void_p, c_ulong, ctypes.POINTER(ctypes.c_char_p)]
X11.XQueryTree.restype = c_int; X11.XQueryTree.argtypes = [ctypes.c_void_p, c_ulong, ctypes.POINTER(c_ulong), ctypes.POINTER(c_ulong), ctypes.POINTER(ctypes.POINTER(c_ulong)), ctypes.POINTER(c_uint)]
X11.XFree.argtypes = [ctypes.c_void_p]

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

dpy = X11.XOpenDisplay(b'localhost:10.0')
if not dpy: sys.exit(1)
root = X11.XDefaultRootWindow(dpy)
w = find_win(dpy, root, b'precursor')
if w: print(hex(w))
" 2>/dev/null)

if [ -z "$WIN" ]; then
    echo "ERROR: Precursor window not found"
    exit 1
fi
echo "Precursor window found: $WIN"

# Run navigation
echo "Starting navigation sequence..."
DISPLAY=localhost:10.0 python3 "$NAVIGATE_SCRIPT"
echo "Navigation complete."

echo "=== Monitoring log for expected entries (60s) ==="
WAIT=0
while [ $WAIT -lt 60 ]; do
    echo "--- t=${WAIT}s ---"
    tail -5 "$LOG"
    # Check for key success indicators
    if grep -q "SwitchToApp\|APP_MENU\|Event::Focus\|account_setup\|Link.*provisioning\|provisioning.*WSS\|connect.*Signal" "$LOG" 2>/dev/null; then
        echo "SUCCESS: Found navigation event in log!"
        grep -E "SwitchToApp|APP_MENU|Event.*Focus|account_setup|provisioning|connect.*Signal|injecting key|Guttering|status:" "$LOG" | tail -30
        break
    fi
    sleep 5
    WAIT=$((WAIT + 5))
done

echo "=== Final log snapshot ==="
tail -40 "$LOG"
