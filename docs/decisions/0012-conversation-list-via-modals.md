---
status: accepted
date: 2026-04-29
---

# 0012 — Conversation list as a modals-driven F1 picker, not a chat-lib screen state

## Context and Problem Statement

Issue #24 (Phase D) calls for a multi-conversation UI: a list of peers
sorted by recency, per-peer unread indicator, per-peer last-message
preview, tap-to-open, back-to-list. The existing `chat-lib` in
`xous-core/libs/chat` only renders a single `Dialogue` at a time —
`Chat::dialogue_set(dict, key)` swaps which one is on screen. The
underlying PDDB schema (one rkyv-serialized `Dialogue` per `pddb_key`
under the app's dialogue dict) already supports per-peer storage; the
gap is purely on the rendering and routing side.

We had to choose between two implementation paths:

A. **Add a list-view screen state to `chat-lib`** — a new
   `ChatScreen::List` enum variant, with a row renderer
   (bold/inverted, leading glyph, unread badge), keyboard navigation
   (↑/↓ to focus, Enter to open), and a back-from-conversation
   gesture (left-arrow when input empty). This is the design the
   `2026-04-26-UI-CONVERSATION-LIST.md` research preserves.

B. **Surface the list as a `modals` radio-button picker triggered
   by F1**, leaving chat-lib unchanged. Selecting a peer calls
   `chat.dialogue_set("sigchat.dialogue", Some(<peer_uuid>))`;
   pressing F1 again returns the user to the picker. Per-peer
   summary metadata (`display_name`, `last_ts`, `last_snippet`,
   `unread`) lives in a new `sigchat.peers` PDDB dict that the
   receive path keeps in sync.

## Decision

We chose **Option B**.

The deciding factor is the project boundary. Option A requires
changes to `xous-core/libs/chat` — i.e., a PR against the locally-
pinned `feat/05-curve25519-dalek-4.1.3` branch, which is upstream
infrastructure shared with `mtxchat` and any future chat skeleton.
Adding a screen-state enum, a row-renderer, and a keyboard-routing
state machine to that lib is multi-session work that has its own
correctness and review burden, and inflates the patch surface this
project already carries on the pinned branch. By contrast, Option B
ships entirely inside `xous-signal-client`'s own crate, uses
chat-lib's existing `dialogue_set` API as-is, and depends only on
`modals` (already a project dependency for account-setup flows).

The cost of Option B is UX coarseness: the picker is the standard
`modals` radio-button widget — single-line labels, no bold/inverted
rendering, no per-row glyphs beyond a leading `*` for unread, no
inline editing or pinning. That's an acceptable MVP because all six
acceptance criteria from issue #24 can still be met:

- **List view enumerating peers from PDDB** —
  `manager::peers::list_sorted()` walks the `sigchat.peers` dict.
- **Per-peer unread indicator** — leading `*` on the picker label;
  reset to space when the user opens that peer.
- **Per-peer last-message preview** — `last_snippet` rendered after
  the display name in the picker label.
- **Tap-to-open conversation** — the radio-button selection maps
  back to a `PeerSummary` and is passed to `SigChat::open_peer`,
  which calls `dialogue_set` to swap chat-lib's active dialogue.
- **Back-from-conversation returns to list** — F1 is bound to
  `show_peer_picker` whenever sigchat receives `Event::F1` from
  chat-lib; pressing F1 from inside any conversation re-opens
  the picker.
- **ADR for the UI architecture (especially how it composes with
  chat-lib)** — this document.

## Consequences

### Per-peer dialogue routing

Inbound `DataMessage`s previously all landed in a single hardcoded
`"default"` dialogue. They now land under the sender's UUID
(`sigchat.dialogue/<uuid>`), with one chat-lib `Dialogue` per peer.
The receive worker keeps a parallel `sigchat.peers/<uuid>` summary
record so the F1 picker has cheap access to recency + unread state
without rkyv-deserializing every peer's full Dialogue.

### Brief flicker on cross-peer inbound

When a message arrives for peer B while the user is viewing peer A,
chat-lib's active dialogue is briefly switched to B (`DialogueSet`),
the post is appended (`PostAdd`), and the dialogue is switched back
to A. Each `DialogueSet` triggers a chat-lib redraw, so the LCD
flickers for the duration of the round trip. The alternative —
writing rkyv-serialized `Dialogue` bytes directly from
`xous-signal-client` — would couple us to an internal chat-lib
struct format that is not part of the lib's public API contract.

### Legacy `default` dialogue is left in place

PDDB snapshots from before this change have everything in
`sigchat.dialogue/default` and a `default.peer` recipient pointer.
Nothing here migrates that data. New inbound messages route per-peer
from this commit forward, and `current_recipient()` prefers the new
`current.peer` pointer when set, falling back to `default.peer`
otherwise. Users with a pre-existing snapshot will see the legacy
log only if they manually `dialogue_set` to `default`; the F1 picker
won't list it.

### Outbound recipient

`SigChat::post` previously sent to whatever was in `default.peer`.
It now uses the focused peer (`current.peer`) when one is set, and
falls back to `default.peer` for sessions where the user hasn't yet
opened anyone via F1 (the V1 most-recent-sender flow).

### What this defers, and what would supersede it

Explicitly out of scope here:

- A bold/inverted scrolling list-view inside chat-lib (Option A).
- F2 contacts pane, F3 new-conversation by username/number,
  F4 settings pane (logout, about). The owner's issue comment
  enumerates these; each warrants its own issue.
- Pin, mark-unread, mute.
- Avatars or initials.
- Migration of pre-existing `default` dialogue contents to per-peer
  dialogues.

A future ADR could supersede this one if Option A becomes worth the
upstream investment — for instance, if a richer information density
becomes a priority (Pebble-style monochrome row layout from the
`2026-04-26-UI-CONVERSATION-LIST.md` research) or if `mtxchat`
independently wants the same feature in chat-lib. The `sigchat.peers`
schema introduced here is the same one a chat-lib screen-state
implementation would consume, so the data model survives the
transition.

## Pointers

- Issue: #24 (Phase D — Conversation list UI).
- Research: `_archive/REPORTS/2026-04-26-UI-CONVERSATION-LIST.md`.
- Related: ADR 0008 (PDDB protocol-stores schema) — the
  `sigchat.peers` dict added here is the seventh `sigchat.*` dict.
