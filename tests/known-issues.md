# Known test failures

This file documents test failures whose root cause is understood but whose
fix is deferred to a dedicated protocol-debugging session. Each entry has
an anchor used by `tools/scan-send.sh` and `tools/run-all-tests.sh` to
label `KNOWN_FAIL` results.

A `KNOWN_FAIL` does not block the overall test suite (the orchestrator
exits 0). It surfaces the failure honestly in the summary output instead
of hiding it as a PASS or SKIP.

When a known issue is fixed, remove the `KNOWN_FAIL` handling from the
relevant scan script, delete the entry here, and update `tests/README.md`.

---

## B2 — signal-cli libsignal decrypt failure after 409-retry ciphertext {#b2-signal-cli-libsignal-decrypt-fail}

**Status:** Open (as of 2026-04-27).

**Symptom.**
After `scan-send.sh` observes the emulator's `post: sent to ...` log line
(leg-1 success), running `signal-cli -a $XSC_RECIPIENT_NUMBER receive`
produces an exception rather than a `Body:` line:

```
Envelope from: "Precursor2" +31653138693 (device: 2) to +31638295471
Timestamp: <ts>
Exception: org.signal.libsignal.protocol.InvalidMessageException:
  invalid Whisper message: decryption failed (ProtocolInvalidMessageException)
```

**Not affected.**
- Receive in the other direction: `scan-receive.sh` (signal-cli → emulator)
  passes cleanly.
- iOS Signal on Precursor1's primary phone: messages from the emulator
  appeared correctly in v6 and v7 scan sessions. signal-cli and iOS Signal
  have independent libsignal implementations; signal-cli is stricter.
- The sync transcript delivered to the emulator's own secondary device
  (device 1 = the emulator itself) also passes — the emulator can read its
  own sent message back.

**Affected leg.**
Leg 2 of the three-legged stool — recipient parse at the protocol layer.
Leg 1 (wire bytes accepted by server) is confirmed PASS. Leg 3 (user-
visible on phone) was confirmed PASS in earlier sessions against iOS Signal.

**Hypothesized cause.**
The emulator's send path executes a 409-retry when Signal-Server reports
`missingDevices=[1]` on the first PUT. During the retry it establishes a
new session with device 1, which advances the ratchet chain counter. The
CIPHERTEXT envelope sent on retry uses a chain index that signal-cli's
libsignal considers out of sync with its own session record (possibly
because signal-cli's session record was last updated during the priming
step, before the retry path advanced the counter on the emulator's side).

The full retry path: `manager/send.rs` → 409 handling → `add_missing` →
`process_prekey_bundle` for device 1 → session established → re-encrypt
all devices → PUT again. The ratchet state written by `process_prekey_bundle`
and the state that signal-cli holds may diverge if the priming-step ciphertext
and the retry-path ciphertext are not strictly ordered in signal-cli's ratchet.

**Evidence.**
- Observed in session 2026-04-27 (Phase R+ / PR #3); confirmed by running
  `signal-cli -a +31638295471 receive` immediately after scan-send PASS.
- The exception message (`decryption failed`) matches libsignal's branch for
  `InvalidMessageException` at the inner `DecryptionCallback`, not a tag or
  padding error — so the envelope framing is valid; the failure is
  specifically in the Double Ratchet decrypt step.
- Scan log shows the correct 409 → retry sequence:
  ```
  send: 409 missing=[1] extra=[] (sent for 1 devices)
  send: ok on attempt 2 (devices=[1, 2])
  ```

**To debug.**
Start a fresh protocol-debugging session with these artifacts in scope:
1. `XSCDEBUG_DUMP=1` wire capture from the failing send — run
   `./tools/decode-wire.sh` and confirm device-2 (signal-cli) ciphertext
   is present and well-formed.
2. signal-cli `--verbose` receive output, which shows the full envelope
   type (should be CIPHERTEXT = type 1, not PREKEY_BUNDLE = type 3 on
   retry) and the ratchet state signal-cli has for that sender+device.
3. The emulator's `XSCDEBUG_DUMP` log for the add_missing path — confirm
   it calls `process_prekey_bundle` for device 2 (signal-cli) during the
   retry, not just device 1.

   If device 2 is NOT getting a prekey-bundle fetch and re-encrypt on retry,
   the fix is in `manager/send.rs`'s `add_missing` logic.
   If device 2 IS getting a fresh bundle, the divergence is in session-record
   persistence between the priming step and the retry.

**When fixed.**
- Remove the `KNOWN_FAIL` leg-2 branch from `tools/scan-send.sh` (the
  `InvalidMessageException` grep and exit 87 path).
- Update `tools/run-all-tests.sh` so exit 87 from scan-send.sh is no longer
  treated as a non-blocking result (it should no longer occur).
- Delete this entry and update `tests/README.md` accordingly.
