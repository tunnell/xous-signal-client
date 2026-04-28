# 0003 — Multi-device send fan-out via `DeviceSessionEnum`

## Status

Accepted.

## Context

Signal accounts commonly have multiple linked devices. When sending
to such an account, the wire body of `PUT /v1/messages/{recipient_uuid}`
must carry one ciphertext per recipient device:

```json
{
  "messages": [
    { "destinationDeviceId": 1, "type": 3, "content": "...", ... },
    { "destinationDeviceId": 2, "type": 1, "content": "...", ... }
  ],
  ...
}
```

Signal-Server returns `409 Mismatched Devices` if the body's
`destinationDeviceId` set differs from the server's registered set
for the recipient (`missingDevices`, `extraDevices`).

The project's V2 implementation (`tunnell/xous-signal-client@089be8e`)
fixed the prekey-bundle base64 decoder (bug arc b001) but the retry
loop only re-encrypted for the original `recipient_addr` device. With
a multi-device recipient, the loop never addressed the missing
device, so `409 missing=[1]` fired three times to exhaustion. (Bug arc
b003.)

## Decision

Implement multi-device send fan-out per the Sesame algorithm:

1. **Enumerate session devices.** `PddbSessionStore::device_ids_for(uuid)`
   parses keys of the form `{uuid}.{device_id}` from the
   `sigchat.session` PDDB dict.
2. **Per-iteration encrypt.** `submit_with_retry_generic` bound:
   `S: SessionStore + DeviceSessionEnum`. Each loop iteration:
   - enumerate device IDs from the session store
   - encrypt the plaintext separately for each device
   - submit one PUT with `messages: Vec<OutgoingMessageEntity>`
3. **409 / 410 handlers update the session store** (drop stale
   sessions for `extraDevices`/`staleDevices`; fetch and process prekey
   bundles for `missingDevices`). The next iteration's enumeration
   picks up the changes naturally.

## Consequences

### Positive

- Sends to multi-device recipients work correctly on the first 409
  recovery iteration.
- Test scaffolding (`StatefulMockHttp`, `TrackingSessionStore`) makes
  the multi-device behavior unit-testable: `StatefulMockHttp` simulates
  Signal-Server's actual 409 behavior (registered devices vs body's
  `messages[]`), so the V3 production bug deterministically reproduces
  in unit tests under the V4 fix's regression test.

### Negative

- The retry loop encrypts on every iteration. This advances the sender's
  ratchet chain counter for every attempt. The V5 audit's hypothesis H5
  (and bug arc b005) speculates that this contributes to or aggravates
  the post-409-retry CIPHERTEXT decrypt failure observed in signal-cli.
  Root cause unconfirmed; tracked as KNOWN_FAIL B2.

### Neutral

- The pattern is adapted from
  `whisperfish/libsignal-service-rs/src/sender.rs::create_encrypted_messages`
  (AGPL-3.0; same license). Their `SessionStoreExt::get_sub_device_sessions`
  corresponds to our `DeviceSessionEnum::device_ids_for`. The reference
  does more (groups, sealed sender, sync transcripts); we mirror only
  the 1:1 multi-device fan-out shape.

## Sources

- `xous-signal-client-notes/bug-arcs/b003-multi-device-fanout.md`
- `xous-signal-client-notes/_extractions/S4.md` (gap surfaced),
  `S5.md` (fix shipped).

## Originating commit

`tunnell/xous-signal-client@ba783ee` "feat(send): multi-device fan-out
for 409/410 retry paths" (PR #1, merged 2026-04-27).
