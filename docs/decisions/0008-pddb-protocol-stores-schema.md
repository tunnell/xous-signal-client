# 0008 — PDDB-backed protocol stores schema

## Status

Accepted. Stores live in `src/manager/stores.rs`.

## Context

libsignal-protocol expresses cryptographic-state persistence via five
async traits:

- `IdentityKeyStore` — own identity + peer TOFU keys.
- `PreKeyStore` — one-time EC prekeys.
- `SignedPreKeyStore` — EC signed prekeys.
- `KyberPreKeyStore` — Kyber1024 prekeys (one-time + last-resort).
- `SessionStore` — Double Ratchet sessions (per peer per device).

The project must persist these to PDDB (Xous's encrypted key-value
store) so cryptographic state survives restart.

## Decision

Five PDDB dicts, one per trait store. Keys, value formats, and
lifecycle below.

### Dict shapes

| Dict | Trait store | Key format | Value | Lifecycle |
|------|-------------|------------|-------|-----------|
| `sigchat.identity` | IdentityKeyStore (peer TOFU) | `{address.name()}.{address.device_id()}` (`.` is unambiguous separator since `name()` is a UUID) | `IdentityKey::serialize()` (33 bytes: 0x05 + 32-byte X25519) | Created on first TOFU contact; overwritten if peer key changes. |
| `sigchat.prekey` | PreKeyStore | decimal `u32::from(PreKeyId)` | `PreKeyRecord::serialize()` | Created on prekey upload; deleted after consumption per `remove_pre_key`. |
| `sigchat.signed_prekey` | SignedPreKeyStore | decimal `u32::from(SignedPreKeyId)` | `SignedPreKeyRecord::serialize()` | One key created at link time; rotation every ~48h is future work. |
| `sigchat.kyber_prekey` | KyberPreKeyStore | decimal `u32::from(KyberPreKeyId)` | `KyberPreKeyRecord::serialize()` | Two keys created at link time (ACI + PNI). **Last-resort prekeys are NOT deleted** — see `mark_kyber_pre_key_used` below. |
| `sigchat.session` | SessionStore | `{address.name()}.{address.device_id()}` | `SessionRecord::serialize()` | Created on first PREKEY_BUNDLE decrypt; updated in place on every message. Never deleted by receive path. |

### Own identity is in `sigchat.account`, not `sigchat.identity`

Own identity keys (private + public) are stored in the existing
`sigchat.account` dict using the existing `aci.identity.private`,
`aci.identity.public`, `pni.identity.private`, `pni.identity.public`,
and `registration_id` keys. `PddbIdentityStore` holds two dict names
(account dict + identity dict) to read own keys from one and peer keys
from the other.

### Read pattern

```rust
let mut pddb_key = pddb.get(dict, key, None, true, false, None, None::<fn()>)?;
let mut buf = Vec::new();
pddb_key.read_to_end(&mut buf)?;
```

`read_to_end` rather than `account.rs`'s fixed-256-byte UTF-8 buffer —
protocol records (especially `SessionRecord`) grow over time.

### Write pattern (delete-then-write)

```rust
pddb.delete_key(dict, key, None).ok();
let mut pddb_key = pddb.get(dict, key, None, true, true, None, None::<fn()>)?;
pddb_key.write_all(&bytes)?;
pddb.sync().ok();
```

Mirrors the `account.rs` pattern.

### `mark_kyber_pre_key_used`

Currently a stub: logs and returns `Ok(())` without deleting the
one-time key or detecting last-resort reuse. Per Signal spec: "a
one-time Kyber pre-key should be deleted after this point. A
last-resort pre-key should check whether the same combination was
used before and produce an error if so."

Tracked as open follow-up (see activeContext.md item 9). Affects
Kyber-prekey-reuse detection but doesn't break the happy path.

### Single-thread ownership

All five stores wrap a `Pddb` handle directly. No `Arc<Mutex>`. The
stores are owned by a single thread (the receive worker — see
ADR 0009).

## Consequences

### Positive

- libsignal-protocol's trait contracts are satisfied by direct PDDB
  storage with no marshalling overhead. No base64, no UTF-8 wrappers,
  just `serialize()` bytes.
- Single-thread ownership avoids cross-thread mutex contention. Pattern
  C (ADR 0009) ensures the receive worker is the only writer.
- Dict prefix `sigchat.*` matches the in-tree app name registered
  in xous-core's app-allowlist (see AGENTS.md "What this is").
  PDDB snapshots persist across rebuilds without migration.

### Negative

- `mark_kyber_pre_key_used` stub means no last-resort reuse detection.
  Open follow-up.
- Five PDDB handles open per envelope dispatch (one per store). This
  is performance polish; tracked as open follow-up but not a correctness
  issue.

### Neutral

- The five-dict schema is informed by libsignal's trait surface. If
  libsignal's traits evolve (e.g., a SenderKeyStore for groups), a new
  dict would be added but existing dicts are stable.

## Sources

- `xous-signal-client-notes/_extractions/S39.md` (storage design).
- `xous-signal-client-notes/_extractions/S41.md` (prekey persistence
  bug fix that wired the link path to write to these dicts).
- `xous-signal-client-notes/bug-arcs/b006-prekey-persistence.md`.

## Originating commits

The five-dict schema was designed during Task 7 Phase 1
(commits `8b874be` / `d3ceb95`, originating in the chat-app
skeleton's pre-fork history). Prekey-private-key persistence came in
commit `39ecba7`. The schema has been stable since.
