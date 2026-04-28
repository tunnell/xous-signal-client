# 0006 — `KNOWN_FAIL` test status convention

## Status

Accepted. Codified in `tests/known-issues.md` and `tools/scan-send.sh`.

## Context

The project has a documented bug (B2 — bug arc b005) — signal-cli
libsignal decrypt failure on post-409-retry CIPHERTEXT — where leg-2
of the three-legged stool of verification (ADR 0005) fails for one
specific recipient (signal-cli) while leg-2 passes for other recipients
(iOS Signal in V6/V7), and leg-1 + leg-3 (manual phone confirmation)
pass.

Three test-orchestrator behaviors are possible for this state:

1. **PASS:** treat leg-1 as sufficient. This is the V3/V4/V5 declared-
   success anti-pattern. Reject.
2. **FAIL:** make B2 block the suite. PRs would be unable to merge until
   B2 is root-caused and fixed. The bug isn't in any of the recently-
   shipped code; it's a long-standing issue. Blocking PRs is overkill.
3. **SKIPPED:** silently hide the failure. Future agents lose the
   institutional knowledge of which leg fails. Reject.

The project needed a fourth category: known-broken, surface honestly,
don't block.

## Decision

Introduce a `KNOWN_FAIL` test status, conveyed via shell exit code 87
from `tools/scan-send.sh`.

### Exit-code semantics

| Code | Meaning |
|------|---------|
| 0 | leg-1 PASS + leg-2 PASS |
| 1 | send FAIL, or leg-2 FAIL with unexpected output |
| 2 | Setup failure |
| 87 | leg-1 PASS + leg-2 KNOWN_FAIL (B2-class output) |

### Orchestrator behavior

`tools/run-all-tests.sh`:

```bash
SEND_EXIT=0
"$SCRIPT_DIR/scan-send.sh" || SEND_EXIT=$?
if (( SEND_EXIT == 0 )); then
    RESULTS[send]="PASS"
elif (( SEND_EXIT == 87 )); then
    RESULTS[send]="KNOWN_FAIL"
    DETAIL[send]="B2: signal-cli libsignal decrypt fail (see tests/known-issues.md)"
elif (( SEND_EXIT == 2 )); then
    RESULTS[send]="SKIPPED"
else
    RESULTS[send]="FAIL"
fi

# ANY_FAIL check is exact string match
ANY_FAIL=0
for r in "${RESULTS[@]:-}"; do
    [[ "$r" == "FAIL" ]] && ANY_FAIL=1
done
exit "$ANY_FAIL"
```

`KNOWN_FAIL` does not equal `FAIL` (exact string match), so the
orchestrator exits 0 when the only non-PASS result is `KNOWN_FAIL`.
The summary line surfaces it explicitly:

```
  send:        KNOWN_FAIL  B2: signal-cli libsignal decrypt fail (see tests/known-issues.md)
```

### Documentation

`tests/known-issues.md` is anchored. Each entry has:

- Status (Open as of `<date>`).
- Symptom (verbatim error output).
- Affected vs not-affected scope.
- Affected leg (which of the three-legged stool).
- Hypothesized cause.
- Evidence.
- Debugging starting point for a future session.
- Cleanup instructions (what to remove from scan scripts +
  orchestrator + this doc when the bug is fixed).

## Consequences

### Positive

- Honest test output. Future agents see `KNOWN_FAIL` and read
  `tests/known-issues.md`; nothing is hidden.
- Non-blocking for unrelated PRs. Work on other parts of the codebase
  isn't held hostage to a single open issue with a known-but-unfixed
  cause.
- Cleanup discipline: when the bug is fixed, the test script and the
  orchestrator are updated alongside the fix, and the entry is deleted.
  Stale `KNOWN_FAIL` entries can't accumulate silently because the
  scan scripts themselves carry the grep-and-exit-87 logic that has to
  be removed.

### Negative

- The bar for "KNOWN_FAIL is acceptable" must stay high. If routine
  bugs start being declared `KNOWN_FAIL` to skip blocking, the category
  loses its meaning. Discipline lives in code review: a `KNOWN_FAIL`
  entry must have a documented hypothesized cause, not just "we don't
  know why."
- The exit code 87 is arbitrary (no convention for "test category
  result" in shell). Documented alongside the implementation.

### Neutral

- The orchestrator's exact-string-match `[[ "$r" == "FAIL" ]]` is the
  load-bearing piece. Any future change to the orchestrator must
  preserve this exactness so KNOWN_FAIL doesn't accidentally become
  blocking.

## Sources

- `xous-signal-client-notes/_extractions/S11.md` (PR #4 implementation).
- `xous-signal-client-notes/bug-arcs/b005-signal-cli-libsignal-decrypt.md`
  (the canonical KNOWN_FAIL bug).
- `tests/known-issues.md` (the in-repo doc).

## Originating commit

`tunnell/xous-signal-client@5117925` "chore: scan-send leg-2 verify +
KNOWN_FAIL convention for B2 (#4)" (PR #4, merged 2026-04-27).
