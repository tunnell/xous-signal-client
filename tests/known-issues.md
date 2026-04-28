# Known test failures

This file documents test failures whose root cause is understood but
whose fix is deferred. Each open entry has an anchor used by
`tools/scan-send.sh` and `tools/run-all-tests.sh` to label results,
plus a resolution checklist for the eventual fix.

A `KNOWN_FAIL` does not block the overall test suite (the orchestrator
exits 0). It surfaces the failure honestly in the summary output
instead of hiding it as a PASS or SKIP. There are no `KNOWN_FAIL`
exit codes wired today — see "Resolved" below.

When a known issue surfaces, add an entry here, wire the detection
into the relevant scan script with exit 87, and add the mapping to
`tools/run-all-tests.sh`.

---

## Currently open

*(none)*

---

## Resolved

### B2 — signal-cli libsignal decrypt failure on emulator's post-409-retry CIPHERTEXT

**Resolved:** 2026-04-28 (issue #8). Closed when three consecutive
`scan-send.sh` runs all PASSed leg-1 + leg-2 with no
`InvalidMessageException` after the receive-direction priming-flake
sibling was fixed in PR #30 (issue #9). The send-direction symptom
last reproduced in 2026-04-27 (PR #4 manifestation) and has not
surfaced since.

**Bug arc:** `xous-signal-client-notes/bug-arcs/b005-signal-cli-libsignal-decrypt.md`
preserves the full investigation history.

**Defensive guard:** `tools/scan-send.sh`'s leg-2 branch still
recognizes the `InvalidMessageException` pattern and surfaces a
clear "B2 regression?" message if it re-occurs, but exits with
status 1 (FAIL) rather than 87 (KNOWN_FAIL). If the pattern returns,
re-open issue #8 with a reproduction.
