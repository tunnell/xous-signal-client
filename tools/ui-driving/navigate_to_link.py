#!/usr/bin/env python3
"""
Navigate Xous hosted mode from idle state all the way through to manager.link().

Full sequence:
  1.  Home  -> open main menu (cursor at Sleep=0)              [wait 1.5s]
  2.  Down  -> move to App (index 1)                           [wait 0.3s]
  3.  Home  -> select App -> App submenu (shellchat=0, signal=1, Close=2) [wait 4.5s]
  4.  Down  -> move to signal (index 1)                        [wait 0.3s]
  5.  Home  -> select signal -> Event::Focus -> account_setup  [wait 7.0s]

  Radio modal (Link/Register/Offline), select_index starts at 0 (Link):
  6.  Home  -> mark Link as selected (action_payload="Link")   [wait 0.3s]
  7.  Down  -> select_index 0→1                                [wait 0.3s]
  8.  Down  -> select_index 1→2                                [wait 0.3s]
  9.  Down  -> select_index 2→3 (OK button)                    [wait 0.3s]
  10. Home  -> confirm Link, modal closes                       [wait 2.5s]

  host_modal (alert_builder, "signal.org" pre-filled, items=[field, OK]):
  11. Down  -> selected item 0→1 (OK button)                   [wait 0.5s]
  12. Home  -> confirm "signal.org", modal closes               [wait 2.0s]

  [probe_host returns true immediately — patched to skip TLS check]

  name_modal (alert_builder, "xous" pre-filled, items=[field, OK]):
  13. Down  -> selected item 0→1 (OK button)                   [wait 0.5s]
  14. Home  -> confirm device name "xous", modal closes         [wait 2.0s]

  manager.link() → WSS connection to Signal provisioning endpoint:
  15. [wait 30s, monitoring log]

Uses XSendEvent (not XTestFakeKeyEvent).
Does NOT call os._exit() — stays alive until manually stopped.
"""

import ctypes, time, os, sys

DISPLAY = os.environ.get("DISPLAY", "localhost:10.0")
LOG = "/home/tunnell/workdir/xous-phase24.log"

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

def tail_log(n=5):
    try:
        with open(LOG) as f:
            lines = f.readlines()
        for l in lines[-n:]:
            print("  LOG:", l.rstrip())
    except:
        pass
    sys.stdout.flush()

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

    focused = c_ulong(0); revert = c_int(0)
    X11.XGetInputFocus(dpy, ctypes.byref(focused), ctypes.byref(revert))
    print(f"Focus: {focused.value:#x} (match={focused.value==win})")

    kc_home = X11.XKeysymToKeycode(dpy, 0xFF50)
    kc_down = X11.XKeysymToKeycode(dpy, 0xFF54)

    print("=== Main menu navigation ===")
    press(dpy, win, root, kc_home, 1.5, "1. Home -> open main menu")
    press(dpy, win, root, kc_down, 0.3, "2. Down -> move to App")
    press(dpy, win, root, kc_home, 4.5, "3. Home -> select App (wait for App submenu)")
    press(dpy, win, root, kc_down, 0.3, "4. Down -> move to signal")
    press(dpy, win, root, kc_home, 7.0, "5. Home -> select signal (wait for radio modal)")

    print("=== Radio modal: mark Link, navigate to OK, confirm ===")
    press(dpy, win, root, kc_home, 0.3, "6. Home -> mark Link (action_payload=Link)")
    press(dpy, win, root, kc_down, 0.3, "7. Down -> select_index 0->1")
    press(dpy, win, root, kc_down, 0.3, "8. Down -> select_index 1->2")
    press(dpy, win, root, kc_down, 0.3, "9. Down -> select_index 2->3 (OK)")
    press(dpy, win, root, kc_home, 2.5, "10. Home -> confirm Link (modal closes)")

    # host_modal: "signal.org" is pre-filled; account.chat_url() prepends chat.
    # automatically, so we just navigate to OK and confirm.
    print("=== host_modal: confirm signal.org (Down to OK, Home to confirm) ===")
    press(dpy, win, root, kc_down, 0.5, "11. Down -> field->OK button")
    press(dpy, win, root, kc_home, 2.0, "12. Home -> confirm signal.org")

    print("=== name_modal: confirm default device name (Down to OK, Home) ===")
    press(dpy, win, root, kc_down, 0.5, "13. Down -> field->OK button")
    press(dpy, win, root, kc_home, 2.0, "14. Home -> confirm device name")

    print("=== QR code should now be visible — scan with Signal on your phone ===")

if __name__ == "__main__":
    main()
