# 0007 — Diagnostic instrumentation, env-var-gated, committed in-tree

## Status

Accepted. Two instrumented paths in production code: `XSCDEBUG_DUMP=1`
(send-side wire-byte capture) and `XSCDEBUG_RECV=1` (receive-side
structured `[recv-debug]` line).

## Context

During the V3-V5 send-debugging arc, a wire-byte capture diagnostic
was written into `outgoing.rs`, used to identify a bug, removed
("the diagnostic isn't part of the production code"), and re-written
again the next session when a different bug needed similar evidence.
This happened **three times** before the V5 audit committed it
permanently behind an env-var gate.

The V6 / V7 receive arc had a similar pattern: an ad-hoc body-content
log line was added during diagnosis, removed before commit (production
logs should be body-free), and re-added next session.

The cost of each "remove and re-add" cycle is real: lost institutional
memory of *why* the log line existed, lost understanding of *what
output to expect*, and lost time in re-discovering edge cases that the
previous diagnostic had surfaced.

## Decision

Diagnostic instrumentation that has paid off twice gets committed
in-tree, behind an environment variable gate, with the format and
location documented.

The two committed instrumented paths:

### `XSCDEBUG_DUMP=1` — wire-byte capture in `outgoing.rs`

Appends labelled hex of the unpadded Content protobuf, padded plaintext,
and per-device ciphertext to `/tmp/xsc-wire-dump.txt`. Read by
`tools/decode-wire.sh` for canonical proto field-tag conformance
checks.

### `XSCDEBUG_RECV=1` — `[recv-debug]` structured log in `main_ws.rs`

Adds a structured log line in `deliver_data_message` and
`deliver_sync_message`:

```rust
if std::env::var("XSCDEBUG_RECV").as_deref() == Ok("1") {
    log::info!(
        "[recv-debug] kind=data author={} ts={} body_len={} body={:?}",
        author, ts, body.len(), body
    );
}
```

Bodies are not logged unless this env var is set; production logs
remain body-free. Consumed by `tools/scan-receive.sh` for marker
round-trip verification.

## The "paid off twice" rule

Before adding new env-var-gated instrumentation, confirm:

1. The diagnostic has been used at least twice across separate
   debugging sessions to identify or characterize a real bug.
2. There's a documented downstream consumer (a test script, a
   debugging document, a tool in `tools/`).
3. The runtime cost when the env var is not set is bounded — at most
   one env-var check per call site per execution.

If a one-off diagnostic is needed for a single session, it's still
fine to add it locally for that session. But it doesn't get committed
unless the second-use bar is cleared.

## Consequences

### Positive

- Future sessions can capture wire bytes (send) or per-message body
  content (receive) by setting an env var and running the standard
  scan scripts. No code changes required for diagnostic work.
- The instrumentation is auditable: anyone reading the production
  source sees `XSCDEBUG_DUMP` / `XSCDEBUG_RECV` paths and knows what
  they emit. There's no hidden debug-only build path.
- Production logs stay body-free by default. The instrumentation
  doesn't leak content unless explicitly enabled.

### Negative

- Slight runtime cost (one env-var check per relevant call site per
  execution). Negligible in practice.
- Code reads with two execution modes: with vs. without the env var.
  Mitigated by keeping the gate at the entry of well-defined functions.

### Neutral

- The convention is `XSCDEBUG_*` for env-var names that affect
  xsc-specific debug output. Reserved namespace.

## Sources

- `xous-signal-client-notes/_extractions/S6.md` (audit — committed
  XSCDEBUG_DUMP after three remove-and-re-add cycles).
- `xous-signal-client-notes/_extractions/S10.md` (PR #3 — committed
  XSCDEBUG_RECV).
- `xous-signal-client-notes/lessons-learned.md` principle 16.
- ADR 0005 Principle 5.

## Originating commits

- `tunnell/xous-signal-client@da08f2e` (PR #1 — committed
  `XSCDEBUG_DUMP` alongside the B1 fix).
- `tunnell/xous-signal-client@86dca0c` (PR #3 — committed
  `XSCDEBUG_RECV`).
