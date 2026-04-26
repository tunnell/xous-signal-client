# Development plan

The path from current pre-alpha to a working 1:1 Signal client on
Precursor.

This plan synthesizes three pieces of design research carried over
from the sigchat fork (now in `docs/research/`):

- **UI design** for the 336×536 monochrome conversation-list screen
  (`docs/research/ui-conversation-list.md`)
- **Memory budget strategy** to hit 1.5 MiB on Precursor
  (`docs/research/memory-budget.md`)
- **Concurrency architecture** for the WebSocket I/O during blocking
  modals (`docs/research/concurrency-architecture.md`)

It also incorporates the UI flow specified by the user for first-run
linking, the post-link conversation list, and function-key
scaffolding.

---

## Current state (post-bootstrap)

Working today:

- Hosted-mode build (`cargo build --features hosted`)
- riscv32imac-unknown-xous-elf build (`--features precursor`)
- 58 library tests passing (manager::send 27, manager::outgoing 2,
  manager::ws_server, manager::rest, manager::prekeys, account
  attribute round-trips). 10 ignored (require Xous IPC runtime).
- Size-budget CI infrastructure (.github + .size-budget.toml)
- i686 sanity build (`RUSTFLAGS=-C relocation-model=static`)
- TESTING-PLAN.md verification discipline
- Linking flow producing a real QR code (manually verified on
  sigchat; ports as-is here)
- xous-ipc 0.10.10 / rkyv 0.8 throughout — same dependency
  versions as xous-core's chat-lib, so the `Buffer` IPC boundary
  no longer crosses a serialization-format gap

Broken or missing:

1. **Keyboard input → `post()` end-to-end verification.** The rkyv
   skew that broke this on sigchat is fixed by construction here
   (matching xous-ipc/rkyv versions). The fix is unverified — needs
   an on-host scan with the existing keyboard-injection script.
2. **First-run linking UI flow.** Per the spec: app opens directly
   to the linking screen on no-account state.
3. **Conversation list screen.** Sorted by recent activity, bold/
   star for unread, connection status indicator. 1:1 only.
4. **Function key routing.** F1=add contact, F2/F3=TBD, F4=settings.
5. **WebSocket worker thread.** Per the concurrency research: a
   single-purpose-SID worker that owns the WS, talks to UI via
   opcode IPC. Required for keepalive when modals block the UI.
6. **Failed-send UI surfacing.** Send failures only log; no marker
   on the message row.
7. **Outbox persistence.** Messages that fail to send are lost on
   restart.
8. **One-time prekey replenishment.** Inbound PREKEY_BUNDLE
   decrypts consume server-side prekeys; the client doesn't refill
   them. Eventually new contacts can't establish sessions.
9. **Size reduction.** Currently 4.0 MiB; target 1.5 MiB. Memory
   research lays out a 14-step plan with two top-level proposals.
10. **Hardware validation.** First Precursor flash and a real
    iPhone account against the live network.

---

## User-specified UI flow

These are not negotiable design decisions; they are requirements.

### First-run / no-account state

App opens directly to the linking screen.

### Linking flow

1. **Intro.** "You are about to link. We will work toward a QR code
   to scan from your phone." Press any key to continue.
2. **Verify cert.** Signal's identity-pinning / cert-trust step;
   user confirms.
3. **Name.** Text-input field for the device name.
4. **QR display.** Render the QR code; user scans it with their
   phone. Press any key when done.
5. **Linked.** "You are linked!" confirmation.

### Post-link state

Standard chat list view.

### Function keys (placeholder layout)

- **F1**: Add new contact
- **F2**: TBD
- **F3**: TBD
- **F4**: Settings (TBD what's there)

### Conversation list

- Sorted by most recent activity, descending.
- 1:1 only for V1 (groups deferred).
- Unread shown via bold or trailing `*`.
- Connection status visible somewhere (status bar).
- Scaffolded for many conversations even though only one is
  expected initially (so sort/scroll can be tested).

---

## Phased plan

### Phase A — Verify the bootstrap (1–2 weeks)

The repo claims to have fixed the rkyv skew that broke
keyboard-input → `post()` on sigchat. Verify it.

- Run `scan-08-local-echo.sh` against the new build and confirm
  the `SigchatOp::Post` handler logs `got SigchatOp::Post,
  s.len()=N` with N > 0 (the log line was added during port).
- Send a test message from `xous-signal-client` (hosted) into
  signal-cli and confirm receipt on the user's phone.
- If the log shows `s.len()=0` or no log at all, surface as a
  finding before any further work — the rkyv hypothesis would
  need re-examination.

**Deliverable:** end-to-end "type a message, see it arrive at
signal-cli" verified on hosted mode against a live linked account.

### Phase B — First-run linking UI (1–2 weeks)

Replace whatever sigchat had at app startup with a state machine
driven by the linking-flow spec.

States and transitions:

```
LinkIntro → LinkVerifyCert → LinkName → LinkQrDisplay → LinkComplete → ChatList
```

- Each state owns its keyhandler. Most advance on any key;
  `LinkName` consumes printable input and submits on Enter;
  `LinkQrDisplay` advances on any key after the user has
  scanned (no scan detection — this is a user-driven advance).
- `LinkComplete` persists account state to PDDB and falls into
  ChatList.
- Already-linked users on cold start skip directly to ChatList.

The existing linking REST flow (`account.rs`,
`manager/libsignal.rs`) is unchanged; only the UI driver wraps
around it.

**Deliverable:** cold start → linked account → ChatList, all
driven from on-device UI.

### Phase C — Conversation list screen (2 weeks)

Per `docs/research/ui-conversation-list.md` and the user's spec.

Concrete sub-tasks:

- Add `ChatScreen::List` state to `libs/chat` (or a sigchat-side
  view if the chat lib's API doesn't accommodate it cleanly —
  decide after reading the upstream chat-lib screen-state enum).
- Define `DialogueSummary { pddb_key, display_name, last_post_*,
  unread_count, pinned, muted }` as the in-memory model.
- Persist `last_post_ts`, `unread_count`, `pinned`, `muted` in a
  separate small PDDB dict (`xous_signal_client.meta`) — never
  scan all Posts at list render time.
- Sort: pinned-first, then by `last_post_ts` desc.
- Re-sort on `SigchatOp::Post` (incoming or outgoing).
- Visual encoding (no color anywhere; 1-bit display):
  - Bold name + bold preview when unread.
  - Leading `●` for unread; trailing filled-rectangle badge
    with the unread count.
  - Full-row inversion (black bg, white fg) for focused row.
  - 1-px horizontal rule between rows.
  - Right-aligned brief relative timestamp ("2m", "Mon").
- Status bar at top: app name, network status, total unread.
- Hint footer at bottom: `↑↓ Select   Home Open   F1 Add   ☰ Menu`.
- Empty state: centered two-line message "No conversations yet.
  Press F1 to add a contact, or Menu to settings." (Initial
  cold-start should never reach this — Phase B routes the
  unlinked user to LinkIntro, the linked user to ChatList with
  at least one auto-created "Notes to self"-style entry, or to
  this empty state if neither.)

Function-key scaffolding lands here:

- F1 → AddContact modal (V1: enter E.164 phone number via the
  existing `modals::get_text` primitive). Creates a new
  `DialogueSummary` and switches to its conversation view.
- F2 / F3 → no-op for now; placeholder routing in the keyhandler.
- F4 → Settings menu (V1 contents: just "Unlink device" and
  "About"; both can be stubs).

**Deliverable:** post-link state shows the conversation list with
working keyboard navigation and unread indicators; F1 path
produces a new conversation.

### Phase D — WebSocket worker thread (1 week)

Per `docs/research/concurrency-architecture.md` (pattern C).

Today, sigchat already has `manager::ws_server` with a
single-purpose-SID worker for the link flow's provisioning WS
(`SignalWsServer::spawn`). Phase D extends this pattern to the
authenticated *receive* WS used post-link, and routes UI ↔ WS
exclusively through the opcode interface so the UI thread can
block in modals (e.g. during AddContact, Settings) without
killing the connection.

- Define an opcode enum mirroring the research recommendation:
  `GetNextFrame` (deferred-response), `Cancel`, `SetKeepalive`,
  `Quit`.
- The worker owns `tungstenite`/`rustls`; `ClientConnection`
  never crosses a thread boundary.
- Worker loop alternates short-timeout WS reads with
  `xous::try_receive_message` checks and ticktimer-driven
  app-level pings (default cadence < 60s, well inside Signal's
  idle timeout).
- UI thread sends `SetKeepalive(25_000)` before opening any
  blocking modal; worker stays connected for the modal's
  duration.

**Deliverable:** the app stays connected during AddContact,
Settings, and link-flow modals. Verified by: open AddContact,
leave it open for 90 seconds, dismiss, send a message — message
arrives without a reconnect cycle.

### Phase E — Send polish (1–2 weeks)

- **Failed-send UI marker.** When `RetryExhausted` or any
  non-retriable error occurs in `manager::send`, surface a `!`
  in the message row's preview line. Drives a chat-lib API
  extension if the lib doesn't already support row-level state
  flags.
- **Outbox persistence.** New PDDB dict
  `xous_signal_client.outbox` keyed by message id. On
  `SigchatOp::Post`, write outbox entry; on send success, delete.
  On startup, scan the outbox and retry pending entries.
- **Retry queue with backoff bumped from per-call to
  outbox-driven.** Per-call retry can drop to 1 attempt once the
  outbox handles long-term retry.

**Deliverable:** send failures are visible and recoverable across
restarts.

### Phase F — Prekey replenishment (1 week)

Tracked from the Task 7 audit.

- After each successful PREKEY_BUNDLE decrypt in `main_ws.rs`,
  enqueue a "replenish" task.
- Replenish task generates new EC one-time prekeys, signs them,
  uploads to `PUT /v2/keys` with `unidentifiedAccess` =
  account's UAK.
- Replenishment threshold: when on-device prekey pool drops below
  20.
- Same path replenishes Kyber one-time prekeys (post-quantum).

**Deliverable:** new contacts can establish sessions
indefinitely; no silent server-side prekey exhaustion.

### Phase G — Size reduction (3–4 weeks)

Per `docs/research/memory-budget.md`. Target: 1.5 MiB total binary.
Current: 4.0 MiB.

Proposal A (keep PQXDH, hit 1.5 MiB by other means) is the
shipping path. Proposal B (drop PQ) is a fallback that would not
work against current Signal servers.

Phased gates from the memory research:

- **G0. Measure baseline.** `cargo bloat --release --target
  riscv32imac-unknown-xous-elf -n 100`, `twiggy top -n 50`,
  `cargo llvm-lines`. Establish current RV32 numbers (the 4.0
  MiB figure is from a precursor build; check whether AVX2
  symbols slipped through).
- **G1. Cheap wins (1 week).** Build-flag profile (opt=z, LTO,
  codegen=1, panic=abort, strip), force curve25519-dalek fiat
  backend, strip libcrux to mlkem1024 only, restrict TLS cipher
  suites. **Gate: −300–500 KB.**
- **G2. TLS dedup (1 week).** Verify `tungstenite` doesn't pull
  its own TLS stack; confirm WS goes through `lib/tls`. **Gate:
  one TLS instance system-wide.**
- **G3. Replace ML-KEM (2 weeks).** Fork `rust/protocol/src/kem`,
  swap `libcrux-ml-kem` for RustCrypto `ml-kem 0.2.x`. Interop
  test against captured Signal handshake before merge. **Gate:
  −350 KB.**
- **G4. Kill `core::fmt` reachability (1 week).** Custom
  panic_handler, `-Cpanic=immediate-abort`, drop production
  Debug derives, replace `format!` with `write!`+heapless
  String. **Gate: −50–150 KB.**
- **G5. Allocator + async (2–3 weeks).** Switch to
  `embedded-alloc` TLSF; bumpalo arenas around handshake/encrypt;
  `heapless::pool::Box` for ratchet state. **Gate: peak RSS
  during TLS handshake < 250 KB above steady-state.**
- **G6. Cull libsignal members (1 week).** Disable `rust/net`,
  `rust/keytrans`, `rust/attest`, `rust/message-backup`,
  `rust/media`, `rust/zkgroup`. **Gate: −100–300 KB.**
- **G7. Migrate Xous TLS provider (2–3 weeks, parallel with
  G5/G6).** Switch `lib/tls` from `ring-xous` to
  `rustls-rustcrypto`. Coordinate with vault/shellchat —
  system-wide change. **Gate: −200–340 KB.**
- **G8. Protobuf tightening + per-PR size budget (1 week +
  ongoing).** Migrate to `micropb` for Signal messages; CI gate
  on `cargo bloat` delta per PR. **Gate: −20–50 KB.**

**Cumulative target:** Phases G1–G7 should land 1.4–1.8 MiB.
Phase G8 keeps it small as features land.

### Phase H — Hardware validation

First Precursor flash. Real iPhone account. Real network.

- Flash device with built image, confirm boot.
- Run linking flow on hardware QR display, scan with the user's
  phone.
- Send a message from the phone, confirm receipt on Precursor.
- Send a message from Precursor, confirm receipt on the phone.
- Leave the device powered on overnight; verify reconnect
  behavior across WiFi blips.

**Deliverable:** "this works on my device" demonstrable.

---

## Deferred (V2+)

Out of scope until V1 ships:

- Group chats (`PUT /v1/messages/multi_recipient`)
- Voice / video
- Stickers
- Stories
- Typing indicators
- Read receipts
- Multi-device support beyond the linked-from phone
- Avatars (monochrome avatars without color de-prioritized per
  UI research)
- Message search / filter
- Quick-jump-to-unread keybinding (post-MVP)

---

## Cross-cutting

- **TESTING-PLAN.md governs every session.** No "done" without
  the four-check report.
- **Size-budget CI runs on every PR** (locally or via GH
  Actions). Per-PR `.text` delta must stay within the per-crate
  caps.
- **Attribution preserved** on every ported file. New files
  carry an SPDX header and a short note on lineage if
  applicable.
- **One-time-SID + opcode IPC** is the canonical concurrency
  pattern. New background workers follow it; `Arc<Mutex<…>>`
  over crypto state is reviewable but discouraged.
- **Single shared crypto provider across the SBOM.** RustCrypto
  is the chosen provider. New features that need crypto reuse
  the existing primitives, not bring their own.

---

## Open questions surfaced during the bootstrap

- The on-disk research docs were re-uploaded by the user during
  this session. Earlier on-disk candidates (TASK-07-display-ui-
  design-question, SIGCHAT-memory-profile, TASK-06b-ws-keepalive-
  design) overlap but don't fully replace the uploaded versions.
  Future sessions should treat `docs/research/` here as
  authoritative.
- Phase A's outcome decides the order of Phases B and C: if the
  rkyv fix needs more work, B/C wait. If it works, B/C can
  proceed in parallel.
- Phase G7 (Xous TLS provider migration) touches xous-core
  itself, not just this repo — coordination with vault and
  shellchat maintainers is required before merge.
