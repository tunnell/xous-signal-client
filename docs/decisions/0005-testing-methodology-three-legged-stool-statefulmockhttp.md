# 0005 — Testing methodology: three-legged stool + StatefulMockHttp

## Status

Accepted. Codified in `tests/README.md`.

## Context

The project's V3-V7 development arc shipped multiple "end-to-end success"
declarations that turned out to be false. V3 declared "send works"
based on a `200 OK` log line; V4 declared "send works end-to-end" based
on a default-valued field in a receipt envelope; V5 audited honestly
and found two new bugs (B1 timestamp tag, B2 decrypt fail).

The pattern: log lines from one component (the local app) were read
as proof of behavior in another component (the recipient's UI). They
were not.

The project also shipped V3 with passing unit tests against a
`ProgrammedMockHttp` that returned the same canned `409 missingDevices=[1]`
forever. The multi-device fan-out bug (recipient_addr fixed across
iterations) deterministically did NOT reproduce in those unit tests
because the mock had no concept of "registered device set."

These two failures share a root: tests passing while production fails.
The methodology codified here addresses both.

## Decision

The project's testing methodology follows six principles, codified in
`tests/README.md`. They are all grounded in specific bugs from the
V3-V7 arc:

### Principle 1 — Mocks must simulate real-server BEHAVIOR, not just
### wire format.

Source: V3 base64-padding bug (canned-response mocks passed; real
Signal-Server used unpadded). V4 single-device-retry bug
(canned-response mocks gave the same reply forever; real server tracks
state).

### Principle 2 — Self-consistent encoders pass tests by being
### bidirectionally wrong.

Source: V6 `DataMessage.timestamp` tag-5-vs-7 bug. All 65 unit tests
passed; iPhone Signal silently dropped at content validation.

### Principle 3 — The three-legged stool of verification.

A send is verified only when **all three** legs hold:
1. **Wire bytes** decode correctly per canonical SignalService.proto.
2. **Recipient parse** (signal-cli, iOS Signal, etc.) reports `Body:`
   for the message.
3. **User-visible** display in the recipient's UI is confirmed.

Source: V3, V4, V5 false success claims based on `200 OK` log lines.

### Principle 4 — Stateful protocols need stateful test doubles.

Source: V4's `StatefulMockHttp` introduction. The mock simulates
Signal-Server's actual 409 behavior: registered devices vs body's
`messages[]`, returns the diff. The V3 production bug becomes a
deterministic test failure under the new fix.

### Principle 5 — Diagnostic instrumentation belongs committed when
### it has paid off twice.

Source: `XSCDEBUG_DUMP=1` written-and-removed three times before being
committed in V5. Same rule applied to `XSCDEBUG_RECV=1` in PR #3.
See ADR 0007.

### Principle 6 — Real-server testing has costs that mock testing
### avoids.

Codifies the project's intentional split: family 1 (Rust unit/integration
tests) in CI; family 2 (hosted-mode E2E send + recv against live
Signal-Server) local-only before PR. Real-server testing has
rate-limit cost (signal-cli linkDevice burns provisioning codes),
real-account state cost, and wire-byte cost.

## Consequences

### Positive

- The "200 OK = success" anti-pattern is named (Principle 3) and made
  structurally impossible: `tools/scan-send.sh` enforces leg-2 via
  `signal-cli receive` after every leg-1 PASS (PR #4, ADR 0006).
- New bugs of the V3-V4-V5 class (canned mocks not catching real-server
  behavior) are caught at unit-test time by `StatefulMockHttp` in
  `src/manager/send.rs::tests`.
- Future contributors run `./tools/run-all-tests.sh` and get a
  pass/fail/skipped report covering all four families (rust, send, recv,
  footprint).

### Negative

- The orchestrator is split across hosted-mode E2E (local-only) and
  unit tests (CI). A contributor who doesn't have the real test
  topology (signal-cli linked, X11 display, linked PDDB snapshot) sees
  E2E SKIPPED, not run. This is intentional; CI-runnable E2E tests
  against real Signal-Server are out of reach.
- The split means a bug in real-server interaction (e.g., bug arc b001
  if it had not been found) could ship to main if no contributor runs
  the local E2E before PR. Mitigation: scan-send.sh's leg-2 enforcement
  + `tools/decode-wire.sh` canonical-tag check are run on every E2E
  pass.

### Neutral

- The methodology is in-repo at `tests/README.md` (416 lines). The
  bug-arc evidence is in
  `xous-signal-client-notes/lessons-learned.md` and
  `xous-signal-client-notes/bug-arcs/`.

## Sources

- `xous-signal-client-notes/_extractions/S9.md` (Phase R — codified).
- `xous-signal-client-notes/_extractions/S6.md` (audit — surfaced the
  declared-success pattern).
- bug arcs b001, b003, b004, b005.

## Originating commit

Commit `7f9b644` (PR #2 "Repository hygiene: testing infrastructure",
merged 2026-04-27).
