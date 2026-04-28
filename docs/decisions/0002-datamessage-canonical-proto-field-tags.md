# 0002 — DataMessage proto field tags follow canonical SignalService.proto

## Status

Accepted.

## Context

The project's manual `prost` proto definitions for `Content`,
`DataMessage`, and `SyncMessage` were originally written by hand based
on partial reference. A bug shipped where `DataMessage.timestamp` was
defined at proto field tag 5 (which is canonically `expireTimer:
uint32`) instead of canonical tag 7 (`timestamp: uint64`). This is
bug arc b004, the V5 audit's "B1".

Symmetric encode/decode bug: both sides used the same
`DataMessageProto` definition with `tag = "5"`. All 65 unit tests
passed because both sides agreed. iPhone Signal silently dropped the
message at content validation. signal-cli rejected with
`InvalidMessageException` at outer decrypt (the wire format mismatch
prevented inner content validation from running).

The cause was clear once the wire bytes were captured: the project's
manual proto definitions were authoritative inside the project, but
disconnected from the canonical Signal `SignalService.proto`.

## Decision

All proto field tags in this project's encoders / decoders MUST match
the canonical `SignalService.proto` upstream (currently visible in
`signalapp/libsignal-service-java/src/main/protowire/SignalService.proto`
and elsewhere in the Signal repos).

Any new field added to a manual `prost` struct definition MUST be
cross-checked against the canonical proto before merging. The
checklist is:

1. Read the canonical `.proto` for the message type.
2. Confirm the field name and tag number match.
3. Confirm the field type matches (e.g., `uint64` vs `uint32`).
4. Add a `tools/decode-wire.sh` regression check if a new wire shape
   is being introduced.

## Consequences

### Positive

- Wire format conformance with the live Signal network. Recipients
  (iOS Signal, Android Signal, signal-cli, libsignal-service-rs-based
  clients) can decode the messages.

### Negative

- Manual proto definitions in `outgoing.rs` and `main_ws.rs` mean this
  decision is enforced by code review and the canonical-tag-conformance
  check in `tools/decode-wire.sh` — not by code generation. A reviewer
  who doesn't open the canonical proto file when editing
  `outgoing.rs::DataMessageProto` could ship the same class of bug
  again. ADR 0001's Consequences section flags this as the strongest
  argument for migrating to libsignal-service-rs (which is
  prost-build-from-canonical-protos by construction).

### Neutral

- The decision constrains future work but does not prescribe an
  implementation. Code-generated protos via `prost-build` would satisfy
  this ADR; manual definitions that match canonical tags also satisfy
  it.

## Field tag reference

| Message | Field | Tag | Type | Source |
|---------|-------|-----|------|--------|
| `Content` | `dataMessage` | 1 | `DataMessage` | SignalService.proto |
| `Content` | `syncMessage` | 2 | `SyncMessage` | SignalService.proto |
| `DataMessage` | `body` | 1 | string | SignalService.proto:357 |
| `DataMessage` | `expireTimer` | 5 | uint32 | SignalService.proto:362 |
| `DataMessage` | `timestamp` | **7** | uint64 | SignalService.proto:365 |
| `DataMessage` | `profileKey` | 6 | bytes | SignalService.proto:364 (not yet set on outbound) |
| `SyncMessage` | `sent` | 1 | `Sent` | SignalService.proto |
| `SyncMessage.Sent` | `timestamp` | 2 | uint64 | SignalService.proto |
| `SyncMessage.Sent` | `message` | 3 | DataMessage | SignalService.proto |
| `SyncMessage.Sent` | `destinationServiceId` | 7 | string | SignalService.proto |

## Sources

- `xous-signal-client-notes/bug-arcs/b004-datamessage-timestamp-tag.md`
- `xous-signal-client-notes/_extractions/S6.md` (audit), `S7.md` (fix).

## Originating commit

`tunnell/xous-signal-client@da08f2e` "fix(proto): DataMessage.timestamp
at canonical tag 7" (PR #1, merged 2026-04-27).
