# Testing xous-signal-client

This document describes the testing methodology used in this project
and how to run all three test families. The methodology section is
not boilerplate — the project's bug history has shaped which test
families exist and what each one is responsible for catching. Read
it before contributing tests.

For the per-check verification discipline that gates every commit
(build, size, i686 sanity, Renode boot smoke, reporting), see
[`TESTING-PLAN.md`](../TESTING-PLAN.md) at the repository root. This
document is the higher-level story that frames why those checks
exist; `TESTING-PLAN.md` is the operational checklist.

## Quick start

Run everything:

```
./tools/run-all-tests.sh
```

The orchestrator runs three test families in order: Rust unit/
integration tests, hosted-mode end-to-end, and memory footprint
(static size + Renode boot smoke). Families whose prerequisites
aren't met (no `tools/.env`, no `signal-cli`, no `renode`) are
reported as **SKIPPED** rather than treated as failures. The exit
code reflects whether all families that COULD run actually passed.

Skip flags are available for selective runs:

```
./tools/run-all-tests.sh --skip-e2e         # rust + footprint only
./tools/run-all-tests.sh --skip-footprint   # rust + e2e
./tools/run-all-tests.sh --skip-renode      # static size only in family 3
```

### KNOWN_FAIL results

A `KNOWN_FAIL` is a failure whose root cause is understood and
documented in [`known-issues.md`](known-issues.md). The orchestrator
exits 0 when all non-`KNOWN_FAIL` results pass — `KNOWN_FAIL` is
surfaced honestly in the summary without blocking the suite.

The conventions:

- A scan script exits with code **87** when it detects a documented
  known-issue pattern. The orchestrator maps exit 87 → `KNOWN_FAIL`.
- Any other non-zero exit from a scan script is still `FAIL` (exit 1)
  or `SKIPPED` (exit 2).
- When a known issue is resolved, delete its entry from `known-issues.md`
  and remove the KNOWN_FAIL handling from the relevant scan script.

There are no current `KNOWN_FAIL` patterns wired today (B2 was the
last one, resolved 2026-04-28; see `known-issues.md` "Resolved"
section). The infrastructure is preserved for future use.

### From a fresh clone

1. Install the Rust toolchain xous-core uses (see `xous-core`'s
   own README for the pinned toolchain). The project pins to
   `feat/05-curve25519-dalek-4.1.3` of `tunnell/xous-core` for path
   dependencies — see `TESTING-PLAN.md` Pre-flight.
2. (Optional, for E2E) Install `signal-cli`, set up two test Signal
   accounts, link them, and populate `tools/.env`. See
   `tools/test-env.example` for the configuration template.
3. (Optional, for footprint) Install the riscv64 binutils
   (`riscv64-unknown-elf-size`, `-readelf`) and `cargo-bloat`. For
   the Renode boot smoke, install Renode v1.16.1 or later.
4. `./tools/run-all-tests.sh`

## Testing methodology

Three test families exist because no single one catches every class
of bug this project has encountered. The Phase A protocol-debugging
arc — four bugs across four sessions in the outbound 1:1 send path —
demonstrated this directly. Each principle below is grounded in a
bug we actually shipped and had to fix.

### Mocks must simulate real-server behavior, not just wire format

The earliest send-path tests used a canned-response queue: a request
came in, a pre-programmed response came back. The shape was
correct, the response codes were plausible, and 39 unit tests
passed. A real send to `chat.signal.org` failed immediately because
Signal-Server's prekey-bundle responses are unpadded base64
(Java's `Base64.getEncoder().withoutPadding()`), and our
`STANDARD.decode` rejected them. The mock had been padded.

The fix introduced `StatefulMockHttp`. Instead of canned responses,
the mock tracks the registered device set for an account UUID, and
its 409 response is computed dynamically from the symmetric
difference between the registered set and the device set in the
request body. A second bug — encrypting only for the original
recipient device on every retry, never picking up the missing
device the 409 told us about — would have passed canned-response
mocks forever, but is caught deterministically by the stateful
mock.

Principle: **a mock that doesn't react like the real server can pass
arbitrarily many tests while leaving production broken in ways the
tests cannot see.**

### Self-consistent encoders pass tests by being bidirectionally wrong

The `DataMessage.timestamp` field on the wire is `tag = 7` per
canonical `SignalService.proto`. Tag 5 in that proto is
`expireTimer (uint32)` — a different field, a different type. The
hand-rolled prost definition in this project had `timestamp` at
tag 5, in both the send-side `DataMessageProto` and the
symmetric receive-side definition.

All 65 unit tests passed. The receive-path round-trip tests passed
because the project's own decoder also read tag 5; sender and
receiver agreed on a non-canonical wire format. iPhone Signal's
`EnvelopeContentValidator` rejects DataMessages without timestamp at
tag 7 and silently drops the message at content validation —
invisible from the sender's side. signal-cli (used in this
project's E2E loop) surfaces the rejection as
`Invalid content! [DataMessage] Missing timestamp!`, which is how
the bug was eventually caught.

Principle: **self-consistent encoder/decoder pairs pass tests
forever. Validation against a canonical reference — either a real
client's parser, or `protoc --decode_raw` against the canonical
`.proto` — is the only way to catch this class of bug.** Family 2
(hosted E2E with signal-cli verify) and `tools/decode-wire.sh`
(canonical proto field-tag check) exist because of this.

### The three-legged stool of verification

A `200 OK` from `PUT /v1/messages` proves the server accepted the
ciphertext. It does not prove anything was delivered, decrypted,
or rendered. Three sessions in the Phase A arc declared "send
works" based on log lines that read:

```
INFO: post: sent to <recipient-uuid>
```

None of those messages reached a recipient phone. Recipients
silently dropped them at content validation (the timestamp tag-5
bug above), or the retry loop never actually addressed the missing
device, or signal-cli's libsignal returned `invalid Whisper
message: decryption failed` and we read its decision-receipt
envelope as proof of delivery.

The verification rule the project now uses has three legs:

1. **Wire bytes** match the canonical Signal protobuf format —
   verified offline via `tools/decode-wire.sh` against a captured
   `XSCDEBUG_DUMP=1` trace.
2. **Recipient parse** succeeds at the protocol layer — verified by
   `signal-cli receive` showing `Body: <text>` for a non-sealed
   recipient.
3. **User-visible confirmation** — a phone or another Signal client
   shows the message as the user expects (incoming on the
   recipient's primary device, outgoing on the sender's primary via
   sync transcript).

Family 2 (hosted E2E) covers legs 1 and 2 automatically. Leg 3 is
human, by design — there is no automated substitute for "I see the
message in my Signal app." Sessions that declare success without
all three legs are wrong until proven otherwise.

### Stateful protocols need stateful test doubles

Signal's multi-device fan-out is a stateful protocol. The sender
must encrypt one ciphertext per device of the recipient's account;
the server returns 409 with `missingDevices` and `extraDevices` if
the body's device list doesn't match the account's actual device
list; the sender then fetches prekey bundles for missing devices,
processes them to establish sessions, drops sessions for extras,
and retries with the new device list.

A mock that returns one canned 409 followed by 200 misses the
retry-and-re-enumerate logic. The session store changes between
attempts; the device list changes between attempts; the new list
must come from session enumeration on each iteration, not from a
captured value at the top of the loop. Several variants of the
single-device-retry bug existed in this codebase and slipped
through canned-response tests for weeks.

`StatefulMockHttp` simulates that behavior: the mock holds a
registered device set, the 409 it returns reflects the actual
diff against the registered set, and a subsequent retry is
checked against the same state. Adding new bug classes is then a
matter of registering a different device set in setup, not adding
a new canned response.

### Diagnostic instrumentation belongs in the codebase

`XSCDEBUG_DUMP=1` is an environment-variable-guarded hex log of the
Content protobuf, padded plaintext, and per-device ciphertexts in
`src/manager/outgoing.rs`. It was first written as an uncommitted
patch during a wire-byte audit, removed, then re-added the next
session for the next bug, then removed again, and finally
committed.

When ad-hoc audit instrumentation isn't in the repository, every
new protocol-correctness investigation pays the cost of writing
it. When it is, future sessions can `XSCDEBUG_DUMP=1 ./run.sh`
and feed the result to `tools/decode-wire.sh`. The runtime cost
when not enabled is one environment variable check per send.

Principle: **diagnostic infrastructure that has paid off twice
should be committed.**

### Real-server testing has costs that mock testing avoids

Hosted-mode E2E tests cannot run in CI without exposing account
credentials. They send real traffic on a real network, are subject
to rate limits, and require human verification of leg 3. They take
2–5 minutes per run vs. ~30 seconds for the Rust family. For
day-to-day development, the Rust family catches most regressions;
hosted E2E is the gate before declaring a protocol change complete.

This project's split is:

- **Family 1** (Rust): runs every commit, in CI, deterministically.
- **Family 2** (hosted E2E): runs locally before opening a PR for a
  protocol-touching change. Not in CI.
- **Family 3** (footprint): runs in CI for static size; Renode boot
  smoke runs locally on an ad-hoc basis.

## Test families

### Family 1: Rust unit and integration tests

**Run:**
```
cargo test --features hosted
```

**Validates:** protocol-level logic against in-process mocks.
Includes the multi-device fan-out logic, 409 / 410 retry handling,
sync transcript construction (`SyncMessage::Sent`), sealed-sender
encryption wrapping, ISO-7816 padding, the unpadded-base64 codec,
and canonical proto field-tag conformance for the round-trip path.

**Does not validate:** real-server behavior, real cryptographic
round-trips against a different libsignal implementation, or the
UI. Per the methodology section, self-consistent encoder bugs and
mock/server divergences pass this family and require Family 2.

**Where the tests live:** inline `#[cfg(test)] mod tests` modules
at the bottom of source files (idiomatic Rust). The current
inventory includes 65 tests across `manager::send`,
`manager::outgoing`, `manager::rest`, `manager::ws_server`, and
`manager::stores`.

### Family 2: Hosted-mode end-to-end tests

This is the manual end-to-end loop exercised before declaring a
protocol-touching change ready to ship. The tooling automates the
emulator drive, wire capture, signal-cli send/verify, and log
correlation; the human confirms leg 3 via Signal apps on physical
phones (only required for the send test's user-visible
confirmation).

The Family 2 family has TWO sub-tests covering both directions of
1:1 messaging:

- **Send (`scan-send.sh`):** xous-emulator → signal-cli (+ phone)
- **Receive (`scan-receive.sh`):** signal-cli → xous-emulator

Both scripts use the same emulator install and the same signal-cli
install — only the direction of the message flow differs. Both
scripts run a `signal-cli listDevices` topology check before
sending anything; if the expected linked secondary isn't present
they exit 2 (setup error) without doing any work. This is the
hard guard against the topology confusion that affected several
earlier sessions.

**Priming step (both scripts):** before either scan boots the
emulator, the script clears signal-cli's stored sessions for the
emulator's UUID via `xsc_clear_signal_cli_sessions` in
`tools/test-helpers.sh`, then sends a priming envelope from
signal-cli to the emulator account. Clearing first forces
signal-cli to issue a PreKey-bundle envelope (type 3) instead of
reusing a stale session and sending a SignalMessage (type 1) —
which the rolled-back PDDB cannot decrypt. This is the documented
B2-sibling priming-flake mitigation; without it, scan-send and
scan-receive flake intermittently. See bug arc `b005` and tracker
issue #9. `tools/demo-prep.sh` uses the same helper for the same
reason.

**Topology (canonical, see `~/workdir/ACCOUNT-MAPPING.md`):**

- Two phone numbers, each on a separate physical phone the user
  owns.
- xous-emulator (this hosted binary) is linked as a secondary on
  the SENDER account (`XSC_SENDER_NUMBER` in `tools/.env`).
- signal-cli on the dev machine is linked as a secondary on the
  RECIPIENT account (`XSC_RECIPIENT_NUMBER`), registered with the
  device name `signal-cli-test`.
- For the receive test, the roles SWAP: signal-cli is the sender
  and the emulator is the receiver. The same `tools/.env` values
  drive both — `scan-receive.sh` swaps internally.

**Setup (one-time):**

1. Pick two Signal accounts you control. They must be different
   phone numbers.
2. Link `signal-cli` as a secondary device on the recipient
   account:
   ```
   signal-cli link -n "signal-cli-test"
   # Scan the printed tsdevice:// URL from your phone's Signal
   # app under Settings > Linked devices.
   signal-cli -a <recipient_number> listDevices
   # Verify TWO devices: phone (primary, Name: null) + Name:
   # signal-cli-test.
   ```
   The Name string `signal-cli-test` is what the topology check
   greps for. Use a different name only if you also update
   `xsc_verify_linked_device` calls in `scan-send.sh` and
   `scan-receive.sh`.
3. Link xous-signal-client as a secondary device on the sender
   account, and capture a PDDB snapshot of the linked state. The
   linking flow is in DEVELOPMENT-PLAN.md / Task 6b history; the
   snapshot lives at
   `xous-core/tools/pddb-images/hosted-linked-display-verified.bin`
   by default.
4. Copy the env template and fill in your values:
   ```
   cp tools/test-env.example tools/.env
   $EDITOR tools/.env
   ```
   `XSC_SENDER_NUMBER` is the emulator account; `XSC_RECIPIENT_NUMBER`
   is the signal-cli account.

**Run a send test:**

```
./tools/scan-send.sh                # sends "Test"
./tools/scan-send.sh "Hello world"  # custom text
```

The script restores the linked PDDB snapshot, boots Xous in hosted
mode, navigates the emulator UI, types the message, presses Enter,
and watches the scan log for `post: sent to ...`. Wire bytes are
captured to `/tmp/xsc-wire-dump.txt` via the `XSCDEBUG_DUMP=1`
environment variable.

After confirming `post: sent to ...` (leg-1), the script waits 8
seconds, runs `signal-cli -a "$XSC_RECIPIENT_NUMBER" receive`, and
looks for `Body: <message>` in the output. This is leg-2 — it
confirms the ciphertext decrypted correctly at the protocol layer.
The script exits 0 only when both legs pass.

**Exit codes from scan-send.sh:**

| Code | Meaning |
|------|---------|
| 0 | leg-1 PASS + leg-2 PASS |
| 1 | leg-1 FAIL, or leg-2 FAIL (any reason) |
| 2 | Setup failure (missing env, prerequisite, topology) |
| 87 | Reserved for documented `KNOWN_FAIL` patterns (currently unused — see `known-issues.md`) |

**Current expected state of leg-2.** Both legs pass cleanly. B2
(signal-cli libsignal `InvalidMessageException` on the emulator's
post-409-retry CIPHERTEXT) was resolved 2026-04-28 — see
`known-issues.md` "Resolved". `scan-send.sh` retains a defensive
recognizer for the B2 pattern; if it ever re-occurs, the script
emits a "B2 regression?" diagnostic and exits 1 (FAIL).

**Verify wire bytes (leg 1):**

```
./tools/decode-wire.sh
```

Reports the structure of each captured Content protobuf and runs
the canonical-tag conformance checks: DataMessage has `body` at
tag 1 and `timestamp` at tag 7; SyncMessage.Sent has `timestamp`
at tag 2, the inner DataMessage at tag 3, and `destinationServiceId`
at tag 7. The script also flags multiple distinct timestamps
across the captured artifacts — a single send should reuse one
timestamp value across five wire locations
(DataMessage.timestamp, sealed-sender envelope timestamp, PUT body
top-level timestamp, SyncMessage.Sent.timestamp, and the
sync-wrapped DataMessage.timestamp).

**Verify recipient parse (leg 2):**

`scan-send.sh` now runs this step automatically after confirming
leg-1. If you need to run it manually after a scan: `signal-cli
-a "$XSC_RECIPIENT_NUMBER" receive`. Confirm a line of the form
`Body: <your test message>`. signal-cli's `--verbose` mode shows
the full envelope path and is useful for diagnosing partial
failures (e.g., `org.signal.libsignal.protocol.InvalidMessageException:
invalid Whisper message: decryption failed` indicates a session-
state mismatch, distinct from `Missing timestamp!` which indicates
a content-validation rejection).

**Verify on phone (leg 3):**

Open Signal on both physical phones (sender and recipient
primaries). Confirm the test message appears: incoming on the
recipient's phone, outgoing on the sender's phone (delivered via
the sync transcript).

**Run a receive test:**

```
./tools/scan-receive.sh                # marker contains "Test"
./tools/scan-receive.sh "Hello world"  # custom marker
```

The script:

1. Verifies `signal-cli-test` is linked to `XSC_RECIPIENT_NUMBER`.
2. Restores the linked PDDB snapshot to `hosted.bin`, boots the
   emulator with `XSCDEBUG_RECV=1`, and waits for
   `main_ws: authenticated websocket established`.
3. Calls `signal-cli send -m "<marker> [recv-<TS>]"` from
   `XSC_RECIPIENT_NUMBER` to `XSC_SENDER_NUMBER`. The
   `[recv-<TS>]` substring is unique per run, so the grep won't
   match older log lines that may exist from previous scans.
4. Watches the emulator log for a `[recv-debug] ...` line
   (emitted by `main_ws::deliver_data_message` when
   `XSCDEBUG_RECV=1`) containing the marker substring.

PASS = match within 60s. FAIL with the last 15 `main_ws` log
lines printed otherwise. Setup errors (topology check failure,
missing PDDB image) exit 2.

**Receive instrumentation:** `XSCDEBUG_RECV=1` enables a
structured log line in `main_ws::deliver_data_message` and
`::deliver_sync_message`:

```
[recv-debug] kind=data author=<...> ts=<...> body_len=<N> body=<...>
```

Bodies are NOT logged unless this env var is set; production
logs remain body-free.

**Common failure modes:**

| Symptom | Likely cause |
|---|---|
| `Missing timestamp!` in signal-cli | DataMessage.timestamp at the wrong proto tag (regression of the v6 bug) |
| HTTP 401 from `PUT /v1/messages` | UAK or device-credential issue; check `tools/.env` and `signal-cli listDevices` |
| 409 retry loop never converging | Device enumeration not picking up newly-established sessions (regression of the v4 bug) |
| Recipient receives, sender phone shows nothing | Sync transcript path failing — check the second Content protobuf in the wire dump and re-run `decode-wire.sh` |
| `Invalid padding` in scan log | Base64 mode regression (regression of the v3 bug) |

### Family 3: Memory footprint

The Xous-target binary has a documented per-crate, per-section,
and total budget in `.size-budget.toml`. The CI runs the full
budget check on every PR (`.github/workflows/size-budget.yml`).
Locally:

**Run static measurement:**

```
./tools/measure-size.sh
```

This builds for `riscv32imac-unknown-xous-elf` (with the
`precursor` feature) and runs
`.github/scripts/check_size_budget.py` to report:

- Total LOAD VM (sum of `MemSiz` for all `PT_LOAD` segments) vs
  hard ceiling (currently 1.5 MiB Baosor working-set ceiling).
- Section-level (`.text`, `.rodata`, `.data`, `.bss`) measured vs
  hard.
- Per-crate `.text` (via `cargo bloat --crates`) vs hard. The
  per-crate caps are sized at `measured + 30% headroom`; a
  per-crate breach with growth ≥30% is a stop-the-session
  regression. Smaller deltas are reported in the session log and
  proceed (see `TESTING-PLAN.md` Check 2).

**Run Renode boot smoke (optional):**

```
./tools/measure-renode.sh
```

Builds a Xous image with xous-signal-client, boots it under Renode
v1.16.1+ for up to 90 seconds, and checks for:

- Absence of `panic`, `abort`, `fault`, `exception`, or `FATAL` in
  the boot log.
- Presence of `INFO:xous_signal_client: ...` (binary reached its
  event loop).

This is a smoke test, not a per-feature regression. The Renode
PDDB-format ceremony (`tests/renode/pddb-format.robot`) is a
separate Robot Framework test invoked via `renode-test`; see
`tests/renode/README.md` for that workflow. The smoke test exists
because the alternative — discovering on real hardware that a
recent change panics during init — is much more expensive.

The smoke test currently exits 2 (skip) rather than 0 (PASS): the
LiteX_Timer_32.cs ulong cast that previously blocked Renode is
fixed (issue #13, tunnell/xous-core PR #18), but routing the
emulated binary's UART output to a captureable backend is open
work tracked as issue #34. Once that lands, `INFO:xous_signal_client`
lines will appear in the boot log and the smoke will pass.

## Per-family pros and cons

| Family | Pros | Cons |
|---|---|---|
| Rust unit/integration | Fast, deterministic, CI-able, covers protocol edge cases | Cannot catch mock/server divergence or self-consistent encoder bugs |
| Hosted E2E | Validates real-server behavior; catches encoder bugs Rust tests miss | Requires test accounts; sends real traffic; cannot run in CI |
| Footprint | Catches binary-bloat regressions before hardware testing; static size runs in CI | Static size doesn't capture runtime peak; full validation needs Renode |

## When to run which

- **Every commit (locally):** Family 1. Fast feedback. Also part of
  `TESTING-PLAN.md` Check 1.
- **Before declaring a protocol change complete:** Family 2.
  Required to ship; mock tests are insufficient by design (see
  methodology, "self-consistent encoders").
- **Before declaring a memory-affecting change complete:** Family 3.
  Required if the binary grows.
- **Before opening a PR:** `./tools/run-all-tests.sh`. Single
  command, full report.

## Adding new tests

- **Family 1 (Rust):** add `#[test]` functions in the appropriate
  source file's inline `mod tests`. Prefer `StatefulMockHttp` over
  canned-response mocks for any test that exercises retry,
  reconnection, or device enumeration.
- **Family 2 (E2E):** the framework lives in `tools/`. New
  scenarios become new helpers in `tools/test-helpers.sh` plus a
  new top-level driver script. Anonymized configuration goes in
  `tools/test-env.example`; never commit real account values.
- **Family 3 (footprint):** new per-crate budgets are added to
  `.size-budget.toml`. The check script reads `[budget.crates.*]`
  and applies the listed `hard` ceiling. Caps should be
  `measured + 30% headroom` per the policy in
  `.size-budget.toml::meta.note`.

## See also

- [`../TESTING-PLAN.md`](../TESTING-PLAN.md) — operational
  per-check verification discipline (build, size, i686, Renode
  boot, report).
- [`renode/README.md`](renode/README.md) — Renode + Robot Framework
  test infrastructure (Antmicro pattern).
- [`../.size-budget.toml`](../.size-budget.toml) — current size
  budgets and growth policy.
- `../.github/workflows/size-budget.yml` — CI size check.
