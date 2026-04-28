# tools/ui-driving/

Reference scripts for driving the Xous hosted-mode minifb window via X11
`XSendEvent`. Originally used to navigate from idle through the device-link
flow during the early protocol-debugging arc. Preserved for Phase D
(conversation-list UI) work where similar automation will be needed.

## Scripts

| Script | What it does |
|--------|--------------|
| `navigate_to_link.py` | Full sequence: idle → menu → App → signal → radio modal (Link) → host modal → name modal → `manager.link()`. The most complete reference for the navigation pattern. |
| `continue_to_link.py` | Resumes navigation from the already-open radio modal (Link highlighted). Useful when a partial automation got stuck or when the flow is split across sessions. |
| `autodrive_qr.py` | Drives idle → QR display modal. Stops at QR (does NOT scan). Watches the Xous log for the `device_link_uri:` line as success. |
| `start_and_navigate.sh` | Orchestrator: starts Xous hosted mode, waits for full init, runs `navigate_to_link.py`. References the legacy `xous-phaseN.log` log-file convention; will need path updates before re-running. |

## Status

These scripts target the older sigchat-skeleton codebase's UI flow.
Specific menu structure, modal sequences, key timing, and log-file paths
may have drifted since they were written. Treat them as **reference
material**, not turn-key automation. Read the script, confirm the menu
structure and timing match current behavior, then adapt.

The X11 `XSendEvent` mechanics (the bottom half of each `.py` script —
display open, keysym lookup, send-event construction) are stable and
reusable as-is for any GAM-driven automation.

## When to use

Phase D (conversation-list UI work) will likely need to drive the chat
UI through scenarios that aren't covered by the current
`tools/scan-send.sh` / `tools/scan-receive.sh` priming pattern. These
scripts are the starting point: copy whichever script most closely
matches the target scenario, adapt the navigation sequence, and reuse
the X11 plumbing.

For non-Phase-D work, the current testing infrastructure
(`tools/run-all-tests.sh` + the priming pattern in
`tools/scan-{send,receive}.sh`) is sufficient. UI-driving scripts here
are deliberately not invoked from the orchestrator — they require a
real X11 display and contain timing assumptions that aren't appropriate
for CI.
