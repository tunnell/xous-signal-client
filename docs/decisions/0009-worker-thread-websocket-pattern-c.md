# 0009 — Worker-thread WebSocket: Pattern C (single-purpose SID + opcode interface)

## Status

Accepted. Implemented in `src/manager/main_ws.rs` (receive WS),
`src/manager/ws_server.rs` (provisioning WS), `src/manager/signal_ws.rs`
(WS handle wrapper).

## Context

Signal's protocol relies on a long-lived authenticated WebSocket for
push delivery. The WS must:

- Stay open across user UI activity (the user opening a GAM modal,
  navigating menus, etc.).
- Respond to server pings within a tight window.
- Send app-layer keepalives (`GET /v1/keepalive` over the WS) at
  ~55s intervals.
- Receive Binary frames (envelopes) and dispatch them to decryption.

GAM's modals are blocking by design (deferred-response pattern). The
naïve approach — let the main thread own the WS, drain it between modal
calls — breaks: the WS misses Pings, Signal-Server times out the
connection at ~60s idle.

Four candidate patterns were evaluated (see [S13]
`xous-signal-client-notes/_extractions/S13.md`):

- **Pattern A** — `Arc<Mutex<SignalWS>>`, background reader, mpsc to
  main.
- **Pattern B** — Ping-only timer thread, main thread still owns WS.
- **Pattern C** — Dedicated worker thread + private `xous::SID` + opcode
  interface.
- **Pattern D** — Non-blocking modal pump.

## Decision

Adopt Pattern C. The Xous Book §7.2.3 "Asynchronous Idioms" pattern:

1. **Mint a private `xous::SID`** via `xous::create_server()` for the
   WS worker. Don't expose the main app SID to the worker. This is
   the book idiom and an attack-surface reduction.
2. **Spawn a worker thread** with `std::thread::spawn` that owns the
   `tungstenite` WS handle directly. The TLS `ClientConnection` never
   leaves this thread.
3. **Expose a small opcode interface** to the rest of the app:
   - `GetNextFrame` (deferred-response: main thread sends blocking;
     worker returns when binary frame arrives, a cancel is received,
     or WS closes)
   - `Cancel` (main thread tells worker to unblock the above)
   - `SetKeepalive(interval_ms)` (configures idle ping cadence)
   - `Quit` (worker shuts down and destroys its SID)
4. **Worker loop alternates** short-timeout WS reads (replying to
   server pings, queueing binary frames) with `xous::try_receive_message`
   checks and ticktimer-driven app-level keepalive sends.

### Why not the alternatives

- **Pattern A (Arc<Mutex<WS>>)** works in Rust's type system but TLS
  state is now behind a Mutex. Every `lock()` is audit surface; the
  message-passing pattern is the Xous-idiomatic alternative. Auditability
  matters for a high-assurance device.
- **Pattern B (ping-only timer thread + main owns WS)** is a TLS state
  race. `rustls::ClientConnection` is `!Sync`; reading on the main
  thread while writing a ping on the timer thread is undefined in the
  Rust type system, and even with a Mutex it collapses to Pattern A.
- **Pattern D (non-blocking modal pump)** would require modifying GAM's
  `Modals` API. Out of scope for this project.
- **tokio async runtime** is rejected per mtxchat's history: mtxcli's
  predecessor used tokio and "became very complex." mtxchat was a
  deliberate simplification toward Xous's threading + message-passing
  model.

## Consequences

### Positive

- TLS keys never cross thread boundaries except as outbound byte
  buffers. Auditable by construction.
- The main thread can block on GAM modals freely; the worker keeps the
  WS alive independently.
- Signal's ~60s idle timeout is a non-issue because the worker actively
  sends app-level keepalives on its own schedule.
- The narrow opcode interface is the audit surface; everything outside
  it is unreachable by the UI side. Matches the Xous Book's "single-
  purpose server" attack-surface-reduction pattern.

### Negative

- Two threads to coordinate (main + worker). The opcode protocol must
  be carefully designed to avoid deadlocks. Done; the `GetNextFrame`
  deferred-response + `Cancel` pair is the only blocking opcode.
- WS-protocol Ping every 25s + app-layer keepalive every 55s = some
  overhead. Both required by spec; not negotiable.

### Neutral

- The same pattern applies to the provisioning WebSocket (used during
  device-link flow): `src/manager/ws_server.rs` is the worker; main
  thread blocks on GAM modals while the worker drains TCP/TLS frames
  and replies to server pings.
- `tungstenite` is configured without `rustls-tls-*` features and
  routes through Xous's existing `lib/tls`. No second TLS stack
  bundled.

## Sources

- `xous-signal-client-notes/_extractions/S13.md` (CONCURRENCY-ARCHITECTURE
  research doc).
- `xous-signal-client-notes/_extractions/S38.md` (TASK-06b-ws-keepalive-IMPL —
  the WS-worker implementation).
- `xous-signal-client-notes/lessons-learned.md` principle 17.

## Originating commits

The pattern was designed during the early WS-keepalive investigation
(see `TASK-06b-ws-keepalive-design.md` in
`xous-signal-client-notes/_archive/REPORTS/`, 999 lines). The
implementation shipped across several commits, including the
`4d11ddf` WS auth fix and `42b3382` app-layer keepalive. The shape
has been stable since.
