#!/usr/bin/env python3
"""
Continue navigation from radio modal (Link already marked) through to manager.link().

State when this script starts:
  - Radio modal is open, "Link" is highlighted (action_payload set), select_index=0
  - We need to navigate to the OK button and confirm

Sequence:
  A. Radio modal OK: Down×3 (to OK button at select_index=3) + Home (confirm)
  B. host_modal (text entry, "signal.org" pre-filled): Down + Home (to OK, confirm)
  C. probe_host runs (~10s TLS check, no key needed)
  D. name_modal (text entry, "xous" pre-filled): Down + Home (confirm)
  E. manager.link() starts → WSS to Signal provisioning endpoint

Uses XSendEvent (not XTestFakeKeyEvent).
"""

import ctypes, time, os, sys

DISPLAY = os.environ.get("DISPLAY", "localhost:10.0")

X11  = ctypes.cdll.LoadLibrary("libX11.so.6")
c_ulong = ctypes.c_ulong; c_int = ctypes.c_int; c_uint = ctypes.c_uint

X11.XOpenDisplay.restype = ctypes.c_void_p; X11.XOpenDisplay.argtypes = [ctypes.c_char_p]
X11.XSync.argtypes = [ctypes.c_void_p, c_int]
X11.XDefaultRootWindow.restype = c_ulong; X11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
X11.XKeysymToKeycode.restype = c_uint; X11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, c_ulong]
X11.XGetInputFocus.argtypes = [ctypes.c_void_p, ctypes.POINTER(c_ulong), ctypes.POINTER(c_int)]
X11.XFlush.argtypes = [ctypes.c_void_p]
X11.XFetchName.restype = c_int; X11.XFetchName.argtypes = [ctypes.c_void_p, c_ulong, ctypes.POINTER(ctypes.c_char_p)]
X11.XQueryTree.restype = c_int; X11.XQueryTree.argtypes = [ctypes.c_void_p, c_ulong, ctypes.POINTER(c_ulong), ctypes.POINTER(c_ulong), ctypes.POINTER(ctypes.POINTER(c_ulong)), ctypes.POINTER(c_uint)]
X11.XFree.argtypes = [ctypes.c_void_p]
X11.XSendEvent.restype = c_int; X11.XSendEvent.argtypes = [ctypes.c_void_p, c_ulong, c_int, c_ulong, ctypes.c_void_p]

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
        if n.value:
            X11.XFree(ch)
        for child in children:
            w = find_win(dpy, child, name_b)
            if w:
                return w
    return None

def press(dpy, win, root, kc, wait, label):
    ev = XEvent()
    ev.key.type = 2          # KeyPress
    ev.key.send_event = 1
    ev.key.display = dpy
    ev.key.window = win
    ev.key.root = root
    ev.key.subwindow = 0
    ev.key.time = 0
    ev.key.x = ev.key.y = ev.key.x_root = ev.key.y_root = 0
    ev.key.state = 0
    ev.key.keycode = kc
    ev.key.same_screen = 1

    r = X11.XSendEvent(dpy, win, 1, 1, ctypes.byref(ev))
    X11.XFlush(dpy)
    time.sleep(0.05)
    ev.key.type = 3
    X11.XSendEvent(dpy, win, 1, 2, ctypes.byref(ev))
    X11.XFlush(dpy)
    print(f"  [{label}] kc={kc} r={r}, waiting {wait}s ...")
    sys.stdout.flush()
    time.sleep(wait)

def main():
    dpy = X11.XOpenDisplay(DISPLAY.encode())
    if not dpy:
        print(f"ERROR: cannot open {DISPLAY}", file=sys.stderr); sys.exit(1)
    print(f"Display: {DISPLAY}")

    root = X11.XDefaultRootWindow(dpy)
    win = find_win(dpy, root, b"precursor")
    if not win:
        print("ERROR: Precursor window not found", file=sys.stderr)
        sys.exit(1)
    print(f"Window: {win:#x}")

    kc_home = X11.XKeysymToKeycode(dpy, 0xFF50)
    kc_down = X11.XKeysymToKeycode(dpy, 0xFF54)

    print("=== A. Radio modal: navigate to OK button (select_index 0→3) and confirm ===")
    press(dpy, win, root, kc_down, 0.3, "A1. Down (select_index 0→1)")
    press(dpy, win, root, kc_down, 0.3, "A2. Down (select_index 1→2)")
    press(dpy, win, root, kc_down, 0.3, "A3. Down (select_index 2→3=OK)")
    press(dpy, win, root, kc_home, 2.5, "A4. Home -> confirm Link (radio modal closes)")

    print("=== B. host_modal: text entry pre-filled with 'signal.org', confirm with OK ===")
    press(dpy, win, root, kc_down, 0.5, "B1. Down (field→OK button)")
    press(dpy, win, root, kc_home, 12.0, "B2. Home -> confirm signal.org (wait for TLS probe ~10s)")

    print("=== C. name_modal: text entry pre-filled with 'xous', confirm with OK ===")
    press(dpy, win, root, kc_down, 0.5, "C1. Down (field→OK button)")
    press(dpy, win, root, kc_home, 2.0, "C2. Home -> confirm device name 'xous'")

    print("=== D. manager.link() should now run - WSS connection to Signal ===")
    print("Waiting 30s for network activity...")
    sys.stdout.flush()
    time.sleep(30)

    print("Done. Check xous-phase21.log and network connections.")
    X11.XSync(dpy, 0)
    os._exit(0)

if __name__ == "__main__":
    main()
