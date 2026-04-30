---
status: accepted
date: 2026-04-30
---

# 0013 — One-time prekey replenishment via `PUT /v2/keys`

## Context and Problem Statement

Issue #15 calls for the missing prekey-replenishment flow. Two
related gaps actually exist:

1. **No initial fill.** sigchat's `LinkDeviceRequestBody` carries
   four prekeys (ACI signed, PNI signed, ACI Kyber last-resort,
   PNI Kyber last-resort) but **zero one-time EC prekeys**. After
   a successful link, the server's stock for our ACI is `count = 0`.
   Senders fetching a prekey bundle for our account get a bundle
   with `preKey: null`; sessions still establish (PQXDH via Kyber
   last-resort works) but at degraded forward-secrecy on first
   message.
2. **No replenishment.** Each successful inbound `PreKeyBundle`
   decrypt consumes one of our one-time prekeys server-side. Without
   a refill path, the count never goes up. (For sigchat today the
   count never actually drops because it never went up — but as
   soon as we fix #1 we need #2 for the same reason.)

The `main_ws.rs` PREKEY_BUNDLE arm carries a TODO referencing this
gap.

## Decision

Implement a stateless, threshold-driven replenisher modeled on
`libsignal-service-rs::account_manager::update_pre_key_bundle`:

1. `GET /v2/keys?identity=aci` — server-reported `{ count, pqCount }`.
2. If `count >= PRE_KEY_MINIMUM` (10), no-op.
3. Otherwise generate `PRE_KEY_BATCH_SIZE` (100) fresh X25519 one-time
   prekeys, persist each to `sigchat.prekey/<id>`, then upload them
   in a single `PUT /v2/keys?identity=aci` call.

Triggered once per `Manager::start_receive`. The replenish runs in
a dedicated thread so a slow GET/PUT can't block the receive
worker's spawn.

### Constants

| Name | Value | Rationale |
|---|---|---|
| `PRE_KEY_MINIMUM` | 10 | Matches `libsignal-service-rs` and Signal-Android's threshold. |
| `PRE_KEY_BATCH_SIZE` | 100 | Matches libsignal-service-rs; equals the per-call cap enforced server-side by `SetKeysRequest` validation. |

### Persistent ID counter

A new PDDB key `sigchat.account/aci.next_prekey_id` (decimal-string-
encoded `u32`) tracks the next prekey ID to allocate. On first read
the counter is seeded with a random value in `[1, 1000]` and persisted,
so a fresh device's first-ever batch isn't trivially correlatable
across installs by ID range. Subsequent batches advance the counter
by `PRE_KEY_BATCH_SIZE`. IDs wrap modulo `MEDIUM_MAX = 0x00FF_FFFF`
so id 0 is never used (Signal protocol rule).

Why a persistent counter instead of `random_prekey_id()` (the
existing pattern for the four signed/Kyber keys)? At 100 keys per
replenishment, random allocation in a 24-bit space starts to collide
non-trivially over a long-running install. Sequential-with-counter
matches the reference implementations and avoids the question
entirely.

### What is uploaded vs. what is left alone

`SetKeysRequest` has four fields. Signal-Server's PUT semantics are
**merge, not replace** — `null`/empty fields preserve the existing
server-side stock. We therefore send only `preKeys`:

| Field | This PR sends | Why |
|---|---|---|
| `preKeys` | the 100 fresh one-time EC keys | the whole point |
| `signedPreKey` | omitted | rotated on a separate (time-driven) cadence; not in scope |
| `pqPreKeys` | omitted | sigchat does not yet upload one-time Kyber prekeys; relies on last-resort |
| `pqLastResortPreKey` | omitted | last-resort is set at link time and not yet rotated |

### No rollback on failed upload

If `generate_one_time_prekeys` succeeds (records persisted to PDDB)
but the subsequent `PUT /v2/keys` fails, we leave the persisted
records in place rather than try to delete them. Locally-stored
prekeys whose IDs the server doesn't have are harmless: the server
never advertises them in any prekey bundle, so no peer ever asks
to use them. The next replenish cycle picks up at the advanced
counter and uploads a fresh batch with new IDs. The orphaned
records sit unused; cleaning them up is a follow-up.

This decision is asymmetric on purpose: we prefer "wasted local
storage" over "delete-then-fail-to-delete" race semantics.

### ACI only, for now

Signal-Server's `PUT /v2/keys` takes `?identity=aci|pni`. sigchat's
receive worker only handles ACI envelopes; PNI sessions aren't on
the wire today. The PNI replenish path is a small extension of this
one and is deferred to a follow-up issue.

### Trigger placement

`Manager::start_receive` runs once per WS session. Replenishment
fires once there in a worker thread, so:

- Recovers from any consume-since-last-startup without polling.
- Doesn't add a periodic timer (keeps it simple).
- A reactive trigger ("decrement-on-decrypt → check threshold") is
  a deferred optimization — for sigchat's current 1:1 traffic
  volume, once-per-session is more than enough.
- Failures don't block the receive worker.

## Consequences

### Forward-secrecy improves on first message

After this PR, the very first replenish (immediately after first
`start_receive`) populates the server with 100 one-time prekeys.
From then on, peers initiating a session against our account get a
bundle that includes a one-time EC prekey, restoring full PQXDH
forward-secrecy on the first message. Before this PR, every first
message was forced into the Kyber-last-resort-only path.

### PDDB schema additions

| Dict | Key | Value | Lifetime |
|---|---|---|---|
| `sigchat.account` | `aci.next_prekey_id` | decimal `u32` | persistent; advances by 100 per replenish |
| `sigchat.prekey` | `<id>` | `PreKeyRecord` bytes | per-key; deleted by libsignal on consumption |

`sigchat.prekey` already existed (the `PddbPreKeyStore` was being
written to by libsignal's `process_prekey_signal_message` — but
nothing ever generated keys to live there until now).

### Thread spawn in `start_receive`

Replenishment runs on a fresh `xsc-prekey-replenish` thread so
the GET+PUT (potentially seconds of TLS + REST round-trips) can't
delay the receive worker's spawn. Cost: one short-lived thread per
WS session. Acceptable for sigchat's RAM budget.

### Ureq-based REST, not the `HttpClient` trait from `send.rs`

`manager::rest` uses `ureq` directly without abstraction (matches
the existing pattern there for `put_devices_link` and
`put_accounts_attributes`). Tests inject closures into the
orchestrator (`run_replenish`) instead of mocking `ureq` itself,
which keeps the test boundary at the right layer (orchestrator
behavior, not HTTP transport).

## Test-workflow caveat

Landing this ADR makes any pre-#15 PDDB snapshot eventually
incompatible with `tools/scan-receive.sh` because the server starts
holding prekey IDs whose private halves are not in the snapshot. In
production this is a non-issue (PDDB never rolls back); in the test
harness it surfaces as `InvalidPreKeyId` on the priming envelope.
See `tests/known-issues.md` → "Stale prekey snapshot divergence
after #15 lands" for the symptom, root cause, and mitigation list
(regenerate the snapshot, drain the server stock, or land #21).

## Pointers

- Issue: #15 (Implement one-time prekey replenishment via PUT /v2/keys).
- Reference: `libsignal-service-rs::pre_keys` (`PRE_KEY_MINIMUM`,
  `PRE_KEY_BATCH_SIZE`, `replenish_pre_keys`).
- Server contract:
  `org.whispersystems.textsecuregcm.controllers.KeysController`
  (`@PUT @Path("/v2/keys")` accepting `SetKeysRequest`,
  `@GET @Path("/v2/keys")` returning `PreKeyCount`).
- Related ADR: 0008 (PDDB protocol-stores schema) — this PR adds
  one new key to `sigchat.account` and starts populating
  `sigchat.prekey`.
- Test caveat: `tests/known-issues.md` → "Stale prekey snapshot
  divergence after #15 lands".

## Deferred (follow-up issues, not blockers for this one)

- PNI prekey replenishment (sigchat is ACI-only on the wire today).
- Signed-prekey rotation on a time-driven schedule.
- Kyber one-time prekey upload (sigchat uses last-resort only).
- Reactive replenishment ("decrement-on-decrypt" hook) for hot peers.
- Cleanup of locally-orphaned prekeys (records whose batch failed
  to upload).
- Periodic background timer; today's once-per-`start_receive` is
  sufficient.
