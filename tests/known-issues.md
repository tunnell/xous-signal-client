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

### Stale prekey snapshot divergence after #15 lands

**Symptom:** `scan-receive.sh` fails with `InvalidPreKeyId` on the
priming envelope (and therefore the marker is never delivered, and
the test exits 1). Specifically the emulator log shows:

```
INFO libsignal_protocol::session: processing PreKey message from <uuid>
ERR  libsignal_protocol::session_management: Message from <uuid> failed
     to decrypt; ... 'invalid prekey identifier'
WARN main_ws: SS PREKEY decrypt failed from <uuid>: InvalidPreKeyId
```

**Root cause:** PR #15 (one-time prekey replenishment via
`PUT /v2/keys`) lands the long-missing initial-fill of one-time EC
prekeys. The first emulator session that runs against a previously-
linked snapshot uploads 100 fresh prekeys — and persists their
private records into that session's `hosted.bin`. The test framework
then restores the immutable
`hosted-linked-display-verified.bin` snapshot for the next test,
**discarding those private records**. The server now has 100
prekeys advertised whose private halves do not exist in the
restored snapshot. signal-cli's session-clear (in
`xsc_clear_signal_cli_sessions`) forces a fresh
`PreKeyBundle` fetch, which includes one of those orphaned IDs.
Decrypt fails.

**Why this is post-#15-only:** before #15, our account had zero
one-time EC prekeys server-side. Bundles handed to senders had
`preKey: null`, sessions established via Kyber last-resort only,
and `InvalidPreKeyId` was unreachable. After #15, every client
boot replenishes — diverging the server's stock from any
pre-#15 snapshot.

**In production this cannot happen:** real users do not roll back
their PDDB. The divergence is purely an artifact of the test
workflow's "restore snapshot before every test" step.

**Mitigation paths (any one resolves it):**

1. **Regenerate the snapshot.** Boot the emulator once with PR #15
   merged, let the replenisher run, then copy the resulting
   `hosted.bin` over
   `hosted-linked-display-verified.bin`. The snapshot now includes
   the 100 fresh prekey private records, and subsequent scan-receive
   runs land on those keys deterministically (until they too are
   consumed and the next replenish cycle's keys take over). Single
   one-time op.
2. **Drain the orphaned server stock.** Each scan-receive run that
   happens to land on a fresh-batch ID consumes one orphan from the
   server's view; eventually the server's old-batch stock is
   exhausted and only fresh ones remain. Slow (96 orphans currently
   on the server for the test account; expect ~30+ runs to drain
   given typical bundle-allocation order).
3. **Land issue #21 (session-recovery handler).** On `InvalidPreKeyId`
   the receiver should send a `RetryMessageRequest`, prompting the
   sender to re-encrypt with a different bundle. This is the
   protocol-level fix and the right long-term answer; out of scope
   for #15 itself.

**Defensive guard / tracking:** none yet. `scan-receive.sh` just
exits 1 on this failure today. If we want to surface it as
`KNOWN_FAIL` in the orchestrator output until one of the
mitigations lands, add an `InvalidPreKeyId` recognizer to
`scan-receive.sh` and the exit-87 mapping to `run-all-tests.sh`.
For now this entry is the source-of-truth.

**Pointers:**
- ADR `docs/decisions/0013-prekey-replenishment.md` for the
  replenishment design.
- Issue #15 (this PR's tracker entry).
- Issue #21 (session-recovery handler) — the long-term fix.

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
