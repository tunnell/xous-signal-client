#!/usr/bin/env python3
"""
Auto-drive Xous hosted-mode sigchat from idle to the QR-display modal.

After boot + GAM ready, injects X11 keys to:
  1. Open main menu, navigate to Apps → signal → focus sigchat
  2. Confirm the Link option in the account-setup radio modal
  3. Accept the default "xous" device name (or whatever is pre-filled)

Stops at QR display (does NOT scan). Monitors the xous log for the
`device_link_uri:` line to confirm QR was rendered.

No arguments. Reads $DISPLAY (defaults to localhost:10.0).
"""

import ctypes, time, os, sys, subprocess, re

DISPLAY = os.environ.get("DISPLAY", "localhost:10.0")

X11 = ctypes.cdll.LoadLibrary("libX11.so.6")
c_ulong, c_int, c_uint = ctypes.c_ulong, ctypes.c_int, ctypes.c_uint

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
X11.XQueryTree.argtypes = [
    ctypes.c_void_p, c_ulong,
    ctypes.POINTER(c_ulong), ctypes.POINTER(c_ulong),
    ctypes.POINTER(ctypes.POINTER(c_ulong)), ctypes.POINTER(c_uint),
]
X11.XFree.argtypes = [ctypes.c_void_p]
X11.XSendEvent.restype = c_int
X11.XSendEvent.argtypes = [ctypes.c_void_p, c_ulong, c_int, c_ulong, ctypes.c_void_p]


class XEvent(ctypes.Union):
    class XKeyEvent(ctypes.Structure):
        _fields_ = [
            ("type", c_int), ("serial", c_ulong), ("send_event", c_int),
            ("display", ctypes.c_void_p), ("window", c_ulong), ("root", c_ulong),
            ("subwindow", c_ulong), ("time", c_ulong),
            ("x", c_int), ("y", c_int), ("x_root", c_int), ("y_root", c_int),
            ("state", c_uint), ("keycode", c_uint), ("same_screen", c_int),
        ]
    _fields_ = [("key", XKeyEvent), ("pad", ctypes.c_char * 192)]


def find_win(dpy, root, name_b):
    cname = ctypes.c_char_p()
    X11.XFetchName(dpy, root, ctypes.byref(cname))
    if cname.value and name_b in cname.value.lower():
        return root
    r, p, n = c_ulong(), c_ulong(), c_uint()
    ch = ctypes.POINTER(c_ulong)()
    if X11.XQueryTree(dpy, root, ctypes.byref(r), ctypes.byref(p), ctypes.byref(ch), ctypes.byref(n)):
        children = [ch[i] for i in range(n.value)]
        if n.value:
            X11.XFree(ch)
        for child in children:
            w = find_win(dpy, child, name_b)
            if w:
                return w
    return None


def press(dpy, win, root, kc, wait, label):
    ev = XEvent()
    ev.key.type = 2  # KeyPress
    ev.key.send_event = 1
    ev.key.display = dpy
    ev.key.window = win
    ev.key.root = root
    ev.key.keycode = kc
    ev.key.same_screen = 1
    X11.XSendEvent(dpy, win, 1, 1, ctypes.byref(ev))
    X11.XFlush(dpy)
    time.sleep(0.05)
    ev.key.type = 3  # KeyRelease
    X11.XSendEvent(dpy, win, 1, 2, ctypes.byref(ev))
    X11.XFlush(dpy)
    print(f"  [{label}] kc={kc}, wait {wait}s")
    sys.stdout.flush()
    time.sleep(wait)


def main():
    if len(sys.argv) > 1:
        log_path = sys.argv[1]
    else:
        # pick the most recent autodrive log
        import glob
        candidates = sorted(glob.glob("/home/tunnell/workdir/scan-06b-AUTODRIVE-*.log"), reverse=True)
        log_path = candidates[0] if candidates else None
        if not log_path:
            print("no AUTODRIVE log found; pass path as arg", file=sys.stderr)
            sys.exit(1)

    dpy = X11.XOpenDisplay(DISPLAY.encode())
    if not dpy:
        print(f"cannot open {DISPLAY}", file=sys.stderr); sys.exit(1)
    root = X11.XDefaultRootWindow(dpy)

    win = find_win(dpy, root, b"precursor")
    if not win:
        print("Precursor window not found", file=sys.stderr); sys.exit(1)
    print(f"Window: {win:#x}")
    print(f"Log:    {log_path}")

    kc_home = X11.XKeysymToKeycode(dpy, 0xFF50)
    kc_down = X11.XKeysymToKeycode(dpy, 0xFF54)

    print("=== Phase A: focus sigchat ===")
    press(dpy, win, root, kc_home, 1.5, "1. Home  → open main menu")
    press(dpy, win, root, kc_down, 0.3, "2. Down  → cursor to Apps")
    press(dpy, win, root, kc_home, 4.5, "3. Home  → select Apps (submenu)")
    press(dpy, win, root, kc_down, 0.3, "4. Down  → cursor to signal")
    press(dpy, win, root, kc_home, 8.0, "5. Home  → focus signal (radio modal)")

    print("=== Phase B: confirm Link in radio modal ===")
    # Radio modal: Link=0 selected by default; 3 Downs to OK button; Home to confirm
    press(dpy, win, root, kc_down, 0.3, "6. Down  → 0→1 Register")
    press(dpy, win, root, kc_down, 0.3, "7. Down  → 1→2 Offline")
    press(dpy, win, root, kc_down, 0.3, "8. Down  → 2→3 OK button")
    press(dpy, win, root, kc_home, 2.5, "9. Home  → confirm Link, radio closes")

    print("=== Phase C: accept default device name ===")
    # name_modal alert_builder: field pre-filled 'xous', items=[field, OK]
    press(dpy, win, root, kc_down, 0.5, "10. Down → field 0→1 OK")
    press(dpy, win, root, kc_home, 2.0, "11. Home → confirm 'xous', modal closes")

    print("=== Phase D: wait for QR modal to render ===")
    deadline = time.time() + 25
    qr_seen = False
    while time.time() < deadline:
        try:
            with open(log_path, "r") as f:
                content = f.read()
        except Exception:
            time.sleep(0.5); continue
        m = re.search(r"device_link_uri: (sgnl://[^ ]+)", content)
        if m:
            print(f"  QR rendered: {m.group(1)[:80]}...")
            qr_seen = True
            break
        time.sleep(0.5)

    if not qr_seen:
        print("  QR did NOT render within 25s; check the window state")
        sys.exit(2)

    print()
    print("=== SUCCESS — QR displayed ===")
    print("Xous is blocked on modals.show_notification() awaiting a key press.")
    print("The WS worker is running in parallel and will receive the envelope on scan.")
    print()
    print("Next step (NOT taken by this script):")
    print("  - User scans the QR with phone; phone approves link")
    print("  - User presses ANY KEY on the Precursor to dismiss the notification")
    print("  - Main thread then calls wait_and_take_binary(), gets the envelope,")
    print("    runs ProvisionMessage::decode, generates prekeys, PUTs /v1/devices/link,")
    print("    and persists the account state.")


if __name__ == "__main__":
    main()
