# 0011 — Affirm hand-rolled libsignal-protocol, with stop-loss criteria

## Status

Accepted. 2026-04-28. Closes issue #23. Supersedes the "open
architectural alternative" caveat in ADR 0001's status line.

## Context

[ADR 0001](0001-hand-rolled-libsignal-protocol-orchestration.md)
chose to depend on `signalapp/libsignal`'s `rust/protocol` and
hand-roll the transport, stores, and orchestration. Its status was
left as "Accepted (with open architectural alternative)" — meaning
the migration question was deferred but not settled.

Issue #23 asked to settle. The trigger was that several "would be
free in libsignal-service-rs" items appeared in the open-issue list
all at once: prekey replenishment (#15), post-link
`PUT /v1/accounts/attributes` (#16), capabilities cleanup
(#17, since closed), recovery handler (#21), sealed-sender (#20),
identity-key UX (#22). Each is a session of orchestration-layer
work that a libsignal-service-rs port would inherit.

The forcing question: at what point does the cumulative
orchestration-layer cost cross the migration threshold (3-6
sessions per ADR 0001's audit)?

## Decision

**Stay with hand-rolled, for now.** Affirm ADR 0001's choice. The
"open architectural alternative" caveat is replaced with **explicit
stop-loss criteria** below — concrete signals that should trigger
re-opening the question, rather than vague "re-assess when".

Conversion: ADR 0001 stays as "Accepted"; this ADR lives alongside
it carrying the stop-loss criteria and the closure of #23.

## Why stay (current evidence)

The case for migration in ADR 0001 was: hand-rolling has shipped
four bugs in the V3-V7 arc (b001, b003, b004, b005), three of which
would not have shipped under a libsignal-service-rs port. That
case is real and the bugs are documented.

The case for staying:

1. **The hand-rolled stack works end-to-end as of 2026-04-28.**
   Three consecutive `scan-send.sh` runs PASS leg-1 + leg-2 with
   no `InvalidMessageException`. B2 (the most-cited remaining
   protocol-orchestration bug) was just closed (issue #8). The
   pattern of "find a bug, fix it, ship the next session" is
   working at the project's current pace.

2. **Migration cost is not amortized over a small ask.** The
   open protocol gaps (#15, #16, #20, #21, #22) are individually
   bounded — small-medium per the issue effort estimates.
   Migrating to libsignal-service-rs to "get them for free" pays
   for the migration only if the migration cost is less than the
   sum of those individual costs. ADR 0001's estimate (3-6
   sessions for migration) approximates the sum (one or two
   sessions each for the open protocol gaps). It's a wash.

3. **Cross-compile risk is not yet retired.** libsignal-service-rs
   bundles `boring`/`hyper`/`tokio-tungstenite`. Each needs to
   either compile for `riscv32imac-unknown-xous-elf` or be
   replaced by an adapter. None of these have been verified to
   work on the Xous target. Until at least one of them is, the
   "3-6 sessions" estimate has high variance — could easily be
   10+ if a critical dep is unportable.

4. **The Phase G size-reduction work (#27) is the bigger lever.**
   At 4.1 MiB total (270% of the 1.5 MiB hard target), the
   binary is well over budget. Migrating the protocol layer
   doesn't help size; the libsignal-service-rs deps would *grow*
   the binary, not shrink it. Migration before size work would
   prematurely lock in a larger footprint.

5. **The Stop-loss criteria below provide a real escape hatch.**
   We're not committing to hand-rolled forever — we're committing
   until specific evidence accumulates that says "now".

## Stop-loss criteria — re-open the question if any of these fires

These supersede ADR 0001's "Re-assess when" list. The intent is
to remove ambiguity: any one of these triggers a fresh decision
session.

1. **A 5th hand-rolled-protocol bug ships in production** (i.e.,
   ends up on the `main` branch and is later fixed). Note: V3-V7
   shipped 4. The 5th moves the dial.
2. **Two consecutive sessions spent on protocol orchestration
   work yield <50% completion of their scoped issue.** Pattern of
   "this is harder than expected" repeating.
3. **A bug arc ships under the `bug-arcs/` directory whose root
   cause is "the libsignal-service-rs reference does this
   differently and ours diverged"**. Signal that we're tracking
   the reference manually and slipping.
4. **libsignal-service-rs upstream introduces a build-time
   feature flag** that disables `boring`/`tokio` (e.g.,
   `signalapp/libsignal#284`-style precedent). Removes the
   cross-compile-risk argument.
5. **Phase G (#27) hits its size target** and the cross-compile
   adapter work for libsignal-service-rs deps becomes affordable
   from a binary-size perspective.

If a session opens with two or more of these flagged, the
re-assessment is overdue.

## Consequences

### What works

- Continued forward motion on the open protocol gaps (#15, #16,
  #20, #21, #22) without architectural pause.
- The `bug-arcs/` and `lessons-learned.md` keep documenting the
  cost; the stop-loss criteria above turn that documentation into
  actionable triggers rather than passive notes.

### What we're accepting

- Each of #15, #16, #20, #21, #22 will take a session of bespoke
  protocol-orchestration work. The aggregate cost is real and
  the choice is to pay it gradually.
- The risk of a 5th orchestration-layer bug in that work, which
  the stop-loss criterion #1 above will surface clearly.

## Notes

- This ADR doesn't commit to never migrating. It commits to a
  concrete framework for deciding when.
- Issue #23 is closed by this ADR; future re-opening of the
  question should reference this ADR and update its Status to
  "Superseded by 00NN" rather than amending in-place.

## Sources

- ADR 0001 — the prior decision and its still-canonical analysis.
- Issue #23 — the request to settle.
- Bug arcs `b001` / `b003` / `b004` / `b005` — the four V3-V7 bugs.
- Issues #15, #16, #20, #21, #22 — the open protocol gaps that
  would migrate "for free" but currently aren't.
- Issue #27 — Phase G size-reduction; the dominant constraint.
