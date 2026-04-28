# 0004 — Sync transcripts (`SyncMessage::Sent`) for own-account devices

## Status

Accepted.

## Context

Signal's protocol distinguishes "outbound" messages addressed to a
peer from "sync" messages that mirror your own outbound to your other
linked devices. Without sync transcripts, when account A sends from
device 2 to account B, account A's other devices (device 1, device 3,
etc.) never see the outgoing message in their UI's sent thread.

Signal's reference clients (Signal-Android, signal-cli,
libsignal-service-rs) emit a `Content { syncMessage { sent { ... } } }`
encrypted-and-fan-out for own-account other devices, in addition to
the recipient send.

## Decision

After every successful recipient send, also build and submit a sync
transcript:

1. **Build padded sync Content** (`build_padded_sync_transcript_content`
   in `outgoing.rs`):
   - `Content { syncMessage = SyncMessage { sent = Sent { ... } } }`
   - `Sent.timestamp` (tag 2) = same `timestamp_ms` as the recipient
     send (consistency required).
   - `Sent.message` (tag 3) = the inner `DataMessage` (same body, same
     timestamp).
   - `Sent.destinationServiceId` (tag 7) = the recipient UUID.
2. **Discover own-account devices.** If no own-account sessions exist
   yet, perform an upfront `GET /v2/keys/{own_uuid}/*` to learn the
   device set and establish sessions
   (`discover_and_establish_account_devices`).
3. **Encrypt and fan-out.** Reuse `submit_padded_with_retry_generic`
   with `excluded_device_id` set to self (so the emulator doesn't try
   to send the sync message to itself). Submit to
   `PUT /v1/messages/{own_uuid}`.
4. **Sync failure is non-fatal.** If the sync send fails, log and
   continue — the recipient send succeeded, which is the load-bearing
   part.

## Consequences

### Positive

- Account A's other devices see outbound messages in their sent thread,
  matching the user expectation set by all other Signal clients.
- User confirmed both phones display the test message during V7:
  phone-personal as incoming, phone-work as outgoing (via the sync
  transcript). First non-self-declared user-visible end-to-end success.
- Wire-byte verification: the same `timestamp_ms` value flows through
  five locations consistently (recipient DataMessage.timestamp;
  Sent.timestamp; Sent.message.timestamp; outer SubmitMessagesRequest;
  sealed-sender envelope timestamp). Verified via `XSCDEBUG_DUMP=1`
  capture.

### Negative

- More wire bytes per send. Every send now triggers up to two PUTs
  (recipient + sync) and per-device encryption for own-account devices.
- The sync flow can trigger its own 409 / 410 recovery on own-account
  devices, expanding the surface where bug arc b005 (B2) might recur.
  In practice, the sync-to-self path has not been observed to trigger
  B2's symptom; the bug is specific to the recipient leg.

### Neutral

- The implementation reuses the V4 fan-out machinery
  (ADR 0003) by generalizing the loop to take pre-built padded Content
  bytes plus an optional `excluded_device_id`. Code reuse is high.
- Test infrastructure: `StatefulMockHttp` extended to support the
  wildcard form `/v2/keys/{uuid}/*` returning a merged response over
  all registered devices. Two new tests:
  - `sync_transcript_fans_out_to_own_other_devices`
  - `sync_transcript_skipped_when_own_account_has_no_other_devices`

## Sources

- `xous-signal-client-notes/_extractions/S8.md` (V7 — feature shipped).
- ADR 0002 (canonical proto field tags).
- ADR 0003 (multi-device fan-out — generalized here).

## Originating commit

`tunnell/xous-signal-client@0429924` (commit on `feat/sync-transcripts`
branch; merged via PR #1, 2026-04-27).
