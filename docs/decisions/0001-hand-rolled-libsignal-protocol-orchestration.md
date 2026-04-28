# 0001 — Hand-rolled libsignal-protocol orchestration

## Status

Accepted (with open architectural alternative — see Notes).

## Context

The project needs Signal Protocol cryptography for end-to-end-encrypted
1:1 messaging on Precursor / Xous. Two reference implementations exist
in Rust:

- **`signalapp/libsignal`** (`rust/protocol`): the canonical Signal
  Protocol primitives. Used by Signal-Android, Signal-Desktop, and the
  signalapp-maintained clients. Pure Rust except for ML-KEM (via
  `libcrux_ml_kem`). Workspace contains many crates; we'd consume only
  `rust/protocol`.

- **`whisperfish/libsignal-service-rs`** (a downstream port):
  full-stack Signal-service implementation including REST/WS,
  prekey management, sync transcripts, message sender/receiver,
  account manager. Higher level. Bundles
  `boring`/`hyper`/`tonic`/`tokio-tungstenite` for transport.

The project chose the lower-level option: depend on
`signalapp/libsignal`'s `rust/protocol` and write our own send /
receive / store integration directly.

## Decision

Use `libsignal-protocol` as the cryptographic primitive layer.
Hand-roll the transport (WebSocket + REST), the protocol-store
backings (PDDB), and the higher-level orchestration (multi-device
fan-out, 409/410 recovery, sealed-sender outer decode, sync
transcripts).

## Consequences

### Positive

- **Small dep tree.** `libsignal-service-rs/net` would pull
  `boring`/`hyper`/`tokio` and a third TLS stack. By avoiding it, we
  keep `tungstenite` (no rustls features; ~30 KB and crypto-free)
  routing through Xous's existing `lib/tls`.
- **Direct control of transport.** The Xous-idiomatic worker-thread
  WebSocket pattern (ADR 0009) requires direct ownership of the
  tungstenite handle. libsignal-service-rs's transport is
  tokio-async; integration would either drag in tokio (rejected; see
  ADR 0009) or require non-trivial adaptation.
- **No tokio.** mtxchat's history rejected tokio explicitly ("became
  very complex"). One std::thread::spawn-based executor fits the
  Xous audit model.
- **Future PQXDH and any libsignal cipher updates** cascade through
  `libsignal-protocol` (the dep), not through a wrapper layer we have
  to track.

### Negative

- **Hand-rolling protocol-level concerns is fragile.** Four bugs in
  the V3-V7 arc (Phase A) are direct evidence:
  - **bug arc b001** (V3): base64 padding (encode/decode mismatch
    with Signal-Server).
  - **bug arc b003** (V4): single-device retry vs multi-device fan-out
    (orchestration-layer recovery logic, not a `libsignal-protocol` bug).
  - **bug arc b004** (V5/V6, "B1"): DataMessage.timestamp at proto
    field tag 5 instead of canonical tag 7 (manual `prost` definition
    is an obvious target — code-generated protos cannot have
    tag-number regressions).
  - **bug arc b005** ("B2", currently OPEN as KNOWN_FAIL): signal-cli
    libsignal decrypt failure on post-409-retry CIPHERTEXT. Root cause
    not confirmed.
- **Protocol features that "come for free" in libsignal-service-rs are
  orchestration-layer work here.** Examples: sealed-sender on send (privacy gap;
  not yet implemented); DataMessage.profileKey on outbound (not yet
  set); session-recovery handler for `RenewSessionAction` /
  `SendRetryMessageRequest` envelopes (not yet implemented); one-time
  prekey replenishment via `PUT /v2/keys` (not yet implemented).
- **Manual proto field-tag definitions create class of bugs that
  prost-build-from-canonical-protos prevents** (see lessons-learned
  principle 5: "Self-consistent encoders pass tests by being
  bidirectionally wrong").

### Neutral

- The reference patterns we mirror (1:1 multi-device fan-out, 409
  recovery, sync transcripts) come from
  `whisperfish/libsignal-service-rs/src/sender.rs` (AGPL-3.0; same
  license as this project). We're using the same algorithm shapes
  rewritten for our transport/store layer.

## Notes

The library-migration question stays as an open architectural question.
The honest cost-benefit:

- **Migration cost:** substantial. libsignal-service-rs is async
  (we use sync `block_on`). Dependency tree may pull crates that
  don't compile for `riscv32imac-unknown-xous-elf` — `reqwest`,
  `tokio-tungstenite`, `boring`. Each needs an adapter or replacement.
  Estimated effort 3-6 sessions per the V5 audit.
- **Migration benefit:** elimination of an entire class of wire-format
  and protocol-orchestration bugs. Three of the four V3-V7 bugs would
  not have shipped under a libsignal-service-rs port. The pattern
  "find a bug, fix it, miss another" repeated four times is
  significant evidence.

Track this as an open architectural question. Re-assess when:
- A fifth or sixth hand-rolled-protocol bug surfaces.
- The project is ready for a multi-session refactor with high variance
  on cross-compile risk.
- libsignal-service-rs upstream evolves (e.g., a build-time `pqxdh`
  feature flag for non-Signal clients per
  `signalapp/libsignal#284`-style precedent).

References to the four-bugs-in-arc evidence are intentional — this
ADR's Consequences section is the canonical place where the cost of
hand-rolling is recorded.

## Sources

- `xous-signal-client-notes/_extractions/S6.md` (PHASEA-AUDIT) — the
  most thorough analysis.
- `xous-signal-client-notes/_extractions/S40.md` — line-by-line diff
  vs whisperfish/libsignal-service-rs.
- bug arcs b001 / b003 / b004 / b005 — concrete bug-arc evidence.
- lessons-learned.md principles 5, 6, 7, 20.

## Originating commit

The decision is embodied in the project's structure (which crates it
depends on, where the sender/receiver live) rather than in any single
commit. The orchestration code was the project's first major addition
on top of the chat-app skeleton.
