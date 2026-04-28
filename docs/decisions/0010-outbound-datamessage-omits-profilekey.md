# 0010 — Outbound DataMessage omits profileKey

## Status

Accepted. 2026-04-28. Closes issue #19.

## Context

`DataMessage` in Signal's canonical `SignalService.proto` carries an
optional `profileKey` field at tag 6 — the sender's profile key,
used by recipients to fetch the sender's profile (display name,
avatar, etc.). The hand-rolled prost definition in
`src/manager/outgoing.rs::DataMessageProto` currently includes only
the `body` (tag 1) and `timestamp` (tag 7) fields. `profileKey` is
absent from outbound messages.

The Phase A audit (V5) suspected this absence was cause **S1** — a
delivery problem. Two later sessions (V6 and V7) demonstrated that
iOS Signal renders the emulator's outbound messages correctly
without `profileKey` set; receipt and rendering work end-to-end.
Issue #19 was filed to settle the question: leave it out, or add it.

## Decision

**Leave `profileKey` absent from outbound `DataMessage` for now.**

The audit's framing was wrong: `profileKey` is not part of the
delivery contract. Signal-Android's `DataMessageProcessor` and
iPhone Signal's `DataMessageBuilder` both treat `profileKey` as
purely a profile-fetch hint. Recipients with no prior context for
the sender display the raw E.164 / UUID until they fetch the
profile via a separate path; once fetched, the display name caches.

For xous-signal-client's current scope (1:1 messaging, demo and
testing against known accounts) the missing display name on first
contact is a UX wart, not a correctness problem. Messages reach the
recipient; the body renders. Adding `profileKey` is a future
display-name-UX enhancement, not a delivery fix.

## Consequences

### What works

- Outbound messages reach the recipient and render the body
  correctly on iOS Signal, Signal-Android, and signal-cli (the last
  having pre-resolved our profile from prior contact).
- The wire format stays minimal — only the two fields required for
  delivery.
- No new attack surface from accidentally exposing the profile key
  on messages where it isn't needed.

### What doesn't

- A recipient with no prior context for the sender sees a raw
  UUID or E.164 instead of the sender's display name on the first
  message. Subsequent messages benefit from any profile fetch the
  recipient initiated in the meantime.
- Profile-aware UX flows (group chat membership, contact list
  presence indicators) won't work cleanly until `profileKey` is
  added.

### Upgrade path

When the project decides to ship profile-aware UX:

1. Add `profile_key: Option<Vec<u8>>` at proto tag 6 to the outbound
   `DataMessageProto` in `src/manager/outgoing.rs`.
2. Read `account.profile_key` (stored as base64 in PDDB at
   `sigchat.account/profile_key`), URL_SAFE_NO_PAD-decode to 32
   bytes, attach to outbound DataMessages.
3. Add a unit test for the wire format — verify the field appears
   at tag 6 and contains the decoded 32-byte key.
4. Open a follow-up issue tracking the work; mark this ADR as
   superseded by the new one (MADR convention; ADRs are
   append-only).

## Notes

- Signal's reference implementations attach `profileKey` on **every**
  outbound `DataMessage`, not just first-contact ones. The recipient
  derives the canonical profile key (also via `unidentifiedAccessKey`)
  for sealed-sender bookkeeping; mismatches between the two are an
  error path. For this reason a future implementation should attach
  the profile key consistently rather than only on suspected
  first-contact paths.

- The `profileKey` on `DataMessage` and the `unidentifiedAccessKey`
  on `AccountAttributes` (see `src/manager/account_attrs.rs::derive_unidentified_access_key`)
  are derived from the same 32-byte profile key. The link account
  flow already handles the underlying secret correctly; the missing
  piece is wiring it through the outbound builder.
