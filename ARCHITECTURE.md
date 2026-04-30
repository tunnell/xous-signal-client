# Architecture

Bird's-eye view of `xous-signal-client`. For protocol-level detail and
rationale, see `docs/decisions/`. For testing methodology, see
`tests/README.md`.

## The big picture

```
                                                             ┌──────────────┐
                                                             │ Signal       │
                                                             │ Server       │
                                                             │ (chat.signal │
                                                             │  .org)       │
                                                             └───┬───┬──┬───┘
                                                                 │   │  │
                                            HTTPS (REST)         │   │  │ WSS
                                            for prekey bundles,  │   │  │ (push)
                                            registration, send.  │   │  │
                                                                 ▼   │  ▼
              xous-signal-client process                       ┌──────┴────┐
              ┌───────────────────────────────────────────────┐│  Network  │
              │                                                ││ TCP+TLS+WS│
              │  main.rs                                       │└─────┬─────┘
              │   │                                            │      │
              │   ├── SigChat::post()                          │      │
              │   │   (chat.cf_post_add + chat.cf_redraw)      │      │
              │   │                                            │      │
              │   ├── manager::send::submit_with_retry         │      │
              │   │     fan-out, 409/410, sync transcripts ────┼──────┼─→ ureq → Xous lib/tls
              │   │                                            │      │
              │   ├── account.rs                               │      │
              │   │     identity, registration, auth           │      │
              │   │                                            │      │
              │   └── manager_ws_server thread (private SID)   │      │
              │         │                                      │      │
              │         └── main_ws::run_session ──────────────┼──────┴─→ tungstenite → Xous lib/tls
              │                  receive worker; sealed-sender,│
              │                  PreKey/Whisper decrypt, deliv │
              │                                                │
              │  PddbStores (single-thread ownership)          │
              │  ┌──────────────┬──────────────┬─────────────┐│
              │  │ identity     │ session      │ prekey/SPK/ ││
              │  │              │              │ Kyber       ││
              │  └──────┬───────┴──────┬───────┴──────┬──────┘│
              │         │              │              │       │
              └─────────┼──────────────┼──────────────┼───────┘
                        │              │              │
                ┌───────▼──────────────▼──────────────▼────────┐
                │ pddb (Protected Encrypted Key-Value Store)   │
                │ — xous-core service —                        │
                └──────────────────────────────────────────────┘

                       ┌─────────────────────────────────────┐
                       │ chat lib (xous-core/libs/chat)      │
                       │ ─────────────────────────────────── │
                       │ SigchatOp::Post handler ←── GAM     │
                       │ cf_post_add / cf_redraw     (typed  │
                       │                              input) │
                       └─────────────────────────────────────┘
```

## Major modules

### `src/main.rs`
- Boot path: `SigChat::new()` → `Account::read(PDDB)` → GAM
  registration → `Event::Focus` → `connect()` → start receive worker.
- Handles `SigchatOp::Post` IPC from the chat lib (typed-line input).
- Enters main loop: drains `user_post`, dispatches to `SigChat::post()`.

### `src/lib.rs`
- `SigChat::post()` performs the local echo (`chat::cf_post_add(cid,
  "me", ts, text)` + `chat::cf_redraw(cid)`) and invokes
  `manager::send::submit_with_retry`. Local echo is required because
  Signal's WebSocket does not push back the sender's own messages.

### `src/manager/send.rs`
- The send pipeline. `submit_with_retry_with_stores` orchestrates
  the recipient send + the optional sync-transcript send.
- `submit_padded_with_retry_generic` is the fan-out core: per-iteration,
  enumerate device IDs from `DeviceSessionEnum::device_ids_for(uuid)`,
  encrypt the plaintext per device, submit one PUT with
  `Vec<OutgoingMessageEntity>`. 409 / 410 handlers update the session
  store; the next iteration's enumeration picks up changes.
- Status mapping for HTTP 200/204/401/404/409/410/413/428/429/5xx;
  retry policy of 3 attempts, 30s budget, backoff 500ms · 2^attempt
  cap 4s.
- `HttpClient` trait — production `UreqHttpClient`, tests `MockHttp` /
  `StatefulMockHttp`.

### `src/manager/prekey_replenish.rs`
- One-time EC prekey replenishment orchestrator (issue #15).
  Threshold-driven (`PRE_KEY_MINIMUM=10`,
  `PRE_KEY_BATCH_SIZE=100`); ACI only for now. Triggered once per
  `Manager::start_receive` from a worker thread so it can't block
  the receive loop. The orchestrator is closure-driven (testable
  without HTTP); the production wrapper wires
  `rest::get_keys_status` and `rest::put_keys`. See ADR 0013 for
  rationale and the deferred-features list.

### `src/manager/peers.rs`
- Per-peer conversation-summary metadata for the F1 conversation-list
  UI. Owns the `sigchat.peers` PDDB dict (one tiny JSON record per
  peer UUID with `display_name`, `last_ts`, `last_snippet`, `unread`).
  Updated by the receive path on inbound, by `SigChat::post` on
  outbound, and reset to `unread=0` when the user opens a peer from
  the F1 picker. Architectural rationale: ADR 0012.

### `src/manager/outgoing.rs`
- Per-message Content protobuf assembly, padding (ISO-7816: 0x80 +
  zeros to multiple of 160), and per-device encrypt step.
- Both DataMessage (recipient) and SyncMessage::Sent (own-account)
  Content shapes are built here. Field tags follow canonical
  `SignalService.proto`: DataMessage.body=1, .timestamp=7;
  SyncMessage.sent=1, Sent.timestamp=2, Sent.message=3,
  Sent.destinationServiceId=7; Content.dataMessage=1, .syncMessage=2.

### `src/manager/main_ws.rs`
- The receive worker. `run_session` loops: read one frame at a time
  (with short read timeout to interleave with app-keepalive timer);
  classify Binary vs Ping vs other; dispatch envelope.
- `dispatch_envelope` branches on envelope type:
  `ENVELOPE_UNIDENTIFIED_SENDER (6)` → `sealed_sender_decrypt_to_usmc`
  → branch on inner type → `message_decrypt_signal` (CIPHERTEXT) or
  `message_decrypt_prekey` (PREKEY_BUNDLE);
  `ENVELOPE_CIPHERTEXT (1)` → direct `message_decrypt_signal`;
  `ENVELOPE_PREKEY_BUNDLE (3)` → direct `message_decrypt_prekey`.
- Reconnect with exponential backoff on connection drop.
- App-layer keepalive: `GET /v1/keepalive` over the authenticated WS
  every 55s. WS-protocol Ping every 25s.

### `src/manager/ws_server.rs`
- The provisioning WebSocket worker (separate from `main_ws`).
  Used during device-link flow. Owns its own private `xous::SID`
  and small opcode interface (`GetNextFrame`, `Cancel`, `SetKeepalive`,
  `Quit`). Pattern C from the [S13] CONCURRENCY-ARCHITECTURE research
  doc.

### `src/manager/signal_ws.rs`
- WebSocket client wrapper. tungstenite + Xous lib/tls. Handles the
  101 Switching Protocols handshake with `Authorization: Basic`
  header (not URL query params; see ADR 0008's bug arc reference).

### `src/manager/stores.rs`
- Five PDDB-backed stores implementing libsignal-protocol's trait
  contracts:
  - `PddbIdentityStore` (peer TOFU keys + own identity from
    `sigchat.account`)
  - `PddbPreKeyStore`, `PddbSignedPreKeyStore`, `PddbKyberPreKeyStore`
    (one-time and signed prekeys)
  - `PddbSessionStore` (Double Ratchet state)
- Single-thread ownership (the receive worker owns them). No
  Arc<Mutex>. Read-pattern `read_to_end`; write-pattern delete-then-
  write + `pddb.sync()`.
- `PddbSessionStore::device_ids_for(uuid)` enumerates
  `{uuid}.{device_id}` keys for fan-out.

### `src/account.rs`
- Stored credentials, registration ID, identity key pair, profile
  key. Backed by `sigchat.account` PDDB dict.

### `src/manager.rs`, `src/manager/{rest,prekeys,account_attrs,libsignal}.rs`
- Provisioning + linking flow. `Account::link` calls
  `PUT /v1/devices/link` with `RegisterAsSecondaryDeviceRequest` JSON
  body (per Signal-Android 8.9.0). On success, persists credentials +
  prekey private keys + signed/Kyber records to PDDB.

## Data flow examples

### Send: typed input → recipient + sync transcript

1. User types in GAM input box; presses Enter.
2. GAM forwards line as memory message to chat lib's `gotinput_id`.
3. chat lib forwards to sigchat's `SigchatOp::Post` opcode.
4. `main.rs` decodes buffer, captures into `user_post`.
5. End of message-loop iteration drains `user_post`, calls
   `SigChat::post(text)`.
6. `SigChat::post` runs local echo (`chat::cf_post_add` + redraw).
7. `manager::send::submit_with_retry_with_stores` runs:
   - Enumerate session devices for recipient UUID.
   - Build padded Content (DataMessage with body + canonical timestamp
     tag 7).
   - Encrypt per device.
   - PUT `/v1/messages/{recipient_uuid}` with
     `messages: Vec<OutgoingMessageEntity>`.
   - On 409, fetch missing devices' prekey bundles, process
     `process_prekey_bundle` per missing device, retry.
   - On success, build padded sync Content (SyncMessage::Sent inner
     DataMessage + destinationServiceId), encrypt for own-account
     other devices, PUT `/v1/messages/{own_uuid}`.

### Receive: WS Binary frame → display

1. Receive worker thread is in `run_session` loop.
2. tungstenite reads a Binary frame.
3. Decode `WebSocketMessage` outer wrapper; extract `Envelope`.
4. Branch on envelope type. Common case is type 6 (sealed sender).
5. `sealed_sender_decrypt_to_usmc` validates trust root, decodes
   `UnidentifiedSenderMessageContent`, extracts sender + inner type.
6. Dispatch on inner type — PreKey or Whisper. Decrypt with libsignal-
   protocol; updates session store.
7. Decode plaintext `Content` proto. Strip Signal application-layer
   padding.
8. Branch on Content.dataMessage / .syncMessage / other.
9. For DataMessage: `chat::cf_post_add(cid, author, ts, body)` +
   `chat::cf_redraw(cid)`. The chat server pushes the post into the
   "default" dialogue and triggers a UI repaint.
10. For SyncMessage::Sent: deliver to the same dialogue with the inner
    DataMessage's body, marking it as our own outbound (received via
    sync from another linked device).

## What's NOT here (yet)

- The conversation-list UI is functional but minimal: F1 surfaces a
  `modals` radio-button picker; richer per-row layout (bold-for-
  unread, scrolling list, pin) is deferred. See ADR 0012.
- No F2 contacts pane, F3 new-conversation entry point, F4 settings
  pane (logout, about, username display) — each warrants its own
  issue.
- No outbox persistence; failed sends are local-echoed only.
- No sealed-sender on outbound (privacy gap; envelopes go as type 1/3).
- No DataMessage.profileKey set on outbound.
- (Done in #15) ACI one-time prekey replenishment runs on every
  `start_receive`. PNI replenishment, signed-prekey rotation, and
  one-time Kyber prekey upload are deferred follow-ups.
- No `mark_kyber_pre_key_used` real implementation (last-resort reuse
  detection is a stub).
- No session-recovery handler for libsignal envelopes
  (RenewSessionAction / SendRetryMessageRequest).
- No group messaging.

See AGENTS.md and `docs/decisions/` for the rationale and
`xous-signal-client-notes/activeContext.md` for the full open-follow-up
list.
