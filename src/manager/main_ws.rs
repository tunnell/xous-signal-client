//! Authenticated main WebSocket worker for Signal message receive.
//!
//! Connects to wss://{host}/v1/websocket/ with an Authorization: Basic header
//! (login="{aci}.{device_id}", password as-is), decrypts incoming envelopes,
//! parses Content proto, and delivers text to the Chat UI.
//! Reconnects automatically with 2–64s exponential backoff on disconnect.

#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]

use std::convert::TryFrom as _;
use futures::executor::block_on;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use libsignal_protocol::{
    CiphertextMessageType, DeviceId, PreKeySignalMessage, ProtocolAddress, PublicKey,
    SignalMessage, Timestamp,
    message_decrypt_prekey, message_decrypt_signal,
    sealed_sender_decrypt_to_usmc,
};
use prost::Message as ProstMessage;
use rand::TryRngCore as _;
use std::io;
use std::thread;
use std::time::Duration;
use ticktimer_server::Ticktimer;
use tungstenite::Message;
use xous::CID;

use crate::manager::signal_ws::SignalWS;
use crate::manager::stores::{
    PddbIdentityStore, PddbKyberPreKeyStore, PddbPreKeyStore, PddbSessionStore,
    PddbSignedPreKeyStore,
};

const KEEPALIVE_MS: u64 = 25_000;
// Signal's application-layer WS keepalive interval, per libsignal-service-rs
// (push_service::KEEPALIVE_TIMEOUT_SECONDS = 55s). This is SEPARATE from the
// WS-protocol Ping above: the server tracks application liveness independently
// and does not push queued messages on connections that have not sent one.
const APP_KEEPALIVE_MS: u64 = 55_000;
const APP_KEEPALIVE_PATH: &'static str = "/v1/keepalive";
const READ_TIMEOUT_MS: u64 = 500;

const ACCOUNT_DICT: &'static str = "sigchat.account";
const IDENTITY_DICT: &'static str = "sigchat.identity";
const PREKEY_DICT: &'static str = "sigchat.prekey";
const SIGNED_PREKEY_DICT: &'static str = "sigchat.signed_prekey";
const KYBER_PREKEY_DICT: &'static str = "sigchat.kyber_prekey";
const SESSION_DICT: &'static str = "sigchat.session";

const WS_TYPE_REQUEST: i32 = 1;
const WS_TYPE_RESPONSE: i32 = 2;

const ENVELOPE_CIPHERTEXT: i32 = 1;
const ENVELOPE_PREKEY_BUNDLE: i32 = 3;
const ENVELOPE_UNIDENTIFIED_SENDER: i32 = 6;

// Signal production sealed-sender trust roots (from libsignal-service-rs configuration.rs).
// SenderCertificate is validated against both; rejection by all roots = drop.
const SEALED_SENDER_TRUST_ROOTS: &[&str] = &[
    "BXu6QIKVz5MA8gstzfOgRQGqyLqOwNKHL6INkv3IHWMF",
    "BUkY0I+9+oPgDCn4+Ac6Iu813yvqkDr/ga8DzLxFxuk6",
];

const RECONNECT_BACKOFF_INITIAL_MS: u64 = 2_000;
const RECONNECT_BACKOFF_MAX_MS: u64 = 64_000;

// ---- Inline prost definitions -----------------------------------------------

#[derive(prost::Message)]
struct WsRequestProto {
    #[prost(string, optional, tag = "1")]
    verb: Option<String>,
    #[prost(string, optional, tag = "2")]
    path: Option<String>,
    #[prost(bytes = "vec", optional, tag = "3")]
    body: Option<Vec<u8>>,
    #[prost(uint64, optional, tag = "4")]
    id: Option<u64>,
    #[prost(string, repeated, tag = "5")]
    headers: Vec<String>,
}

#[derive(prost::Message)]
struct WsResponseProto {
    #[prost(uint64, optional, tag = "1")]
    id: Option<u64>,
    #[prost(uint32, optional, tag = "2")]
    status: Option<u32>,
    #[prost(string, optional, tag = "3")]
    message: Option<String>,
    #[prost(bytes = "vec", optional, tag = "4")]
    body: Option<Vec<u8>>,
    #[prost(string, repeated, tag = "5")]
    headers: Vec<String>,
}

#[derive(prost::Message)]
struct WsMessageProto {
    #[prost(int32, optional, tag = "1")]
    r#type: Option<i32>,
    #[prost(message, optional, tag = "2")]
    request: Option<WsRequestProto>,
    #[prost(message, optional, tag = "3")]
    response: Option<WsResponseProto>,
}

// Signal Envelope (signalservice.proto)
#[derive(prost::Message)]
struct EnvelopeProto {
    #[prost(int32, optional, tag = "1")]
    r#type: Option<i32>,
    #[prost(string, optional, tag = "2")]
    source_service_id: Option<String>,
    #[prost(uint32, optional, tag = "7")]
    source_device: Option<u32>,
    #[prost(uint64, optional, tag = "5")]
    server_timestamp: Option<u64>,
    #[prost(bytes = "vec", optional, tag = "8")]
    content: Option<Vec<u8>>,
}

// DataMessage (signalservice.proto)
#[derive(prost::Message)]
struct DataMessageProto {
    #[prost(string, optional, tag = "1")]
    body: Option<String>,
    #[prost(uint64, optional, tag = "5")]
    timestamp: Option<u64>,
}

// SyncMessage.Sent (signalservice.proto)
#[derive(prost::Message)]
struct SentMessageProto {
    // destinationServiceId: ACI of the recipient
    #[prost(string, optional, tag = "1")]
    destination_service_id: Option<String>,
    #[prost(uint64, optional, tag = "2")]
    timestamp: Option<u64>,
    #[prost(message, optional, tag = "3")]
    message: Option<DataMessageProto>,
}

// SyncMessage (signalservice.proto)
#[derive(prost::Message)]
struct SyncMessageProto {
    #[prost(message, optional, tag = "1")]
    sent: Option<SentMessageProto>,
    // Other sub-messages are present on the wire but opaque for Phase 5 —
    // prost silently ignores unknown fields.
}

// Content (signalservice.proto)
#[derive(prost::Message)]
struct ContentProto {
    #[prost(message, optional, tag = "1")]
    data_message: Option<DataMessageProto>,
    #[prost(message, optional, tag = "2")]
    sync_message: Option<SyncMessageProto>,
}

// ---- Public interface -------------------------------------------------------

pub struct MainWsWorker {
    thread: thread::JoinHandle<()>,
}

impl MainWsWorker {
    pub fn spawn(
        aci_service_id: String,
        device_id: u32,
        password: String,
        host: String,
        chat_cid: CID,
    ) -> io::Result<Self> {
        let t = thread::Builder::new()
            .name("sigchat-main-ws".into())
            .spawn(move || worker_loop(aci_service_id, device_id, password, host, chat_cid))
            .map_err(|e| io::Error::other(format!("main_ws spawn: {e}")))?;
        Ok(Self { thread: t })
    }

    #[allow(dead_code)]
    pub fn join(self) {
        let _ = self.thread.join();
    }
}

// ---- Outer reconnect loop ---------------------------------------------------

fn worker_loop(
    aci_service_id: String,
    device_id: u32,
    password: String,
    host: String,
    chat_cid: CID,
) {
    if device_id == 0 || device_id > 127 {
        log::error!("main_ws: device_id {device_id} out of valid Signal range (1..=127)");
        return;
    }
    let local_device = match DeviceId::new(device_id as u8) {
        Ok(d) => d,
        Err(_) => return,
    };
    let local_addr = ProtocolAddress::new(aci_service_id.clone(), local_device);

    let mut backoff_ms = RECONNECT_BACKOFF_INITIAL_MS;

    loop {
        log::info!("main_ws: connecting to {host}");
        match SignalWS::new_message(&host, &aci_service_id, device_id, &password) {
            Ok(ws) => {
                backoff_ms = RECONNECT_BACKOFF_INITIAL_MS;
                log::info!("main_ws: authenticated websocket established");
                run_session(ws, &local_addr, chat_cid);
                log::info!("main_ws: session ended, reconnecting in {}ms", backoff_ms);
            }
            Err(e) => {
                log::warn!("main_ws: connect failed: {e}, retrying in {}ms", backoff_ms);
            }
        }

        if let Ok(tt) = Ticktimer::new() {
            let _ = tt.sleep_ms(backoff_ms as usize);
        }
        backoff_ms = (backoff_ms * 2).min(RECONNECT_BACKOFF_MAX_MS);
    }
}

// ---- Inner session loop -----------------------------------------------------

fn run_session(mut ws: SignalWS, local_addr: &ProtocolAddress, chat_cid: CID) {
    if let Err(e) = ws.set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS))) {
        log::warn!("main_ws: set_read_timeout failed: {e}");
    }

    let tt = match Ticktimer::new() {
        Ok(t) => t,
        Err(e) => {
            log::error!("main_ws: Ticktimer::new failed: {e:?}");
            ws.close();
            return;
        }
    };

    let mut last_ping_ms = tt.elapsed_ms();
    let mut next_request_id: u64 = 1;
    // Fire the application-layer keepalive immediately on connect: libsignal-service-rs
    // uses tokio::time::interval_at(Instant::now(), ...), so its first tick fires at t=0.
    // Without this initial request the server appears to hold queued messages.
    let mut last_app_keepalive_ms = tt.elapsed_ms().saturating_sub(APP_KEEPALIVE_MS);

    loop {
        // (1a) WS-protocol Ping (25s). Useful for NAT/TCP keepalive; the server
        // returns Pong at this layer.
        if tt.elapsed_ms().saturating_sub(last_ping_ms) >= KEEPALIVE_MS {
            match ws.send(Message::Ping(Vec::new())) {
                Ok(()) => {
                    last_ping_ms = tt.elapsed_ms();
                    log::info!("main_ws: sent keepalive Ping");
                }
                Err(e) => {
                    log::warn!("main_ws: keepalive Ping failed: {e}");
                    break;
                }
            }
        }

        // (1b) Signal application-layer keepalive (55s): WsRequestProto
        // { verb=GET, path=/v1/keepalive }. Required for the server to push
        // queued messages on the connection.
        if tt.elapsed_ms().saturating_sub(last_app_keepalive_ms) >= APP_KEEPALIVE_MS {
            let req = WsRequestProto {
                verb: Some("GET".to_string()),
                path: Some(APP_KEEPALIVE_PATH.to_string()),
                body: None,
                id: Some(next_request_id),
                headers: Vec::new(),
            };
            let msg = WsMessageProto {
                r#type: Some(WS_TYPE_REQUEST),
                request: Some(req),
                response: None,
            };
            match ws.send(Message::Binary(msg.encode_to_vec())) {
                Ok(()) => {
                    last_app_keepalive_ms = tt.elapsed_ms();
                    log::info!("main_ws: sent app keepalive (id={next_request_id})");
                    next_request_id = next_request_id.wrapping_add(1);
                }
                Err(e) => {
                    log::warn!("main_ws: app keepalive send failed: {e}");
                    break;
                }
            }
        }

        // (2) Read next WebSocket frame (500ms timeout drives the cycle).
        let raw = match ws.read() {
            Ok(Message::Binary(b)) => {
                log::info!("main_ws: got Binary frame {} bytes", b.len());
                b
            }
            Ok(Message::Text(t)) => {
                log::warn!("main_ws: got TEXT frame (unexpected) len={} preview={:?}",
                           t.len(), &t.chars().take(80).collect::<String>());
                continue;
            }
            Ok(Message::Ping(p)) => {
                log::info!("main_ws: got server Ping ({} bytes)", p.len());
                continue;
            }
            Ok(Message::Pong(p)) => {
                log::info!("main_ws: got server Pong ({} bytes)", p.len());
                continue;
            }
            Ok(Message::Close(c)) => {
                log::info!("main_ws: server closed connection: {c:?}");
                break;
            }
            Ok(other) => {
                log::warn!("main_ws: got unhandled Message variant: {:?}",
                           std::mem::discriminant(&other));
                continue;
            }
            Err(e) if is_timeout(&e) => continue,
            Err(e) => {
                log::warn!("main_ws: read error: {e}");
                break;
            }
        };

        // (3) Decode WebSocketMessage wrapper.
        let ws_msg = match WsMessageProto::decode(raw.as_slice()) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("main_ws: WebSocketMessage decode failed: {e}");
                continue;
            }
        };

        match ws_msg.r#type {
            Some(WS_TYPE_REQUEST) => {
                if let Some(req) = ws_msg.request {
                    handle_request(&mut ws, req, local_addr, chat_cid);
                }
            }
            Some(WS_TYPE_RESPONSE) => {}
            other => log::warn!("main_ws: unhandled WsMessage type {other:?}"),
        }
    }

    log::info!("main_ws: session loop exited; closing websocket");
    ws.close();
}

// ---- Request dispatch -------------------------------------------------------

fn handle_request(
    ws: &mut SignalWS,
    req: WsRequestProto,
    local_addr: &ProtocolAddress,
    chat_cid: CID,
) {
    let id = req.id.unwrap_or(0);
    let verb = req.verb.as_deref().unwrap_or("");
    let path = req.path.as_deref().unwrap_or("");

    if verb == "PUT" && path == "/api/v1/message" {
        if let Some(body) = req.body {
            dispatch_envelope(body, local_addr, chat_cid);
        }
        send_ack(ws, id, 200);
    } else if verb == "PUT" && path == "/api/v1/queue/empty" {
        log::info!("main_ws: message queue drained");
        send_ack(ws, id, 200);
    } else {
        log::info!("main_ws: server request {verb} {path} (id={id})");
        send_ack(ws, id, 200);
    }
}

fn send_ack(ws: &mut SignalWS, id: u64, status: u32) {
    let msg = WsMessageProto {
        r#type: Some(WS_TYPE_RESPONSE),
        request: None,
        response: Some(WsResponseProto {
            id: Some(id),
            status: Some(status),
            message: Some("OK".to_string()),
            body: None,
            headers: Vec::new(),
        }),
    };
    if let Err(e) = ws.send(Message::Binary(msg.encode_to_vec())) {
        log::warn!("main_ws: ACK send failed (id={id}): {e}");
    }
}

// ---- Envelope decryption ----------------------------------------------------

fn dispatch_envelope(body: Vec<u8>, local_addr: &ProtocolAddress, chat_cid: CID) {
    let envelope = match EnvelopeProto::decode(body.as_slice()) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("main_ws: Envelope proto decode failed: {e}");
            return;
        }
    };

    let source_id = envelope.source_service_id.clone().unwrap_or_default();
    let source_dev = envelope.source_device.unwrap_or(1);
    let env_type = envelope.r#type.unwrap_or(0);
    let ts = envelope.server_timestamp.unwrap_or(0);

    log::info!("main_ws: envelope type={env_type} from={source_id}/{source_dev} ts={ts}");

    let content = match envelope.content {
        Some(c) => c,
        None => {
            log::warn!("main_ws: envelope type={env_type} has no content bytes — dropping");
            return;
        }
    };

    let pddb_id = pddb::Pddb::new(); pddb_id.try_mount();
    let pddb_pk = pddb::Pddb::new(); pddb_pk.try_mount();
    let pddb_spk = pddb::Pddb::new(); pddb_spk.try_mount();
    let pddb_kpk = pddb::Pddb::new(); pddb_kpk.try_mount();
    let pddb_ses = pddb::Pddb::new(); pddb_ses.try_mount();

    let mut identity_store = PddbIdentityStore::new(pddb_id, ACCOUNT_DICT, IDENTITY_DICT);
    let mut pre_key_store = PddbPreKeyStore::new(pddb_pk, PREKEY_DICT);
    let signed_pre_key_store = PddbSignedPreKeyStore::new(pddb_spk, SIGNED_PREKEY_DICT);
    let mut kyber_pre_key_store = PddbKyberPreKeyStore::new(pddb_kpk, KYBER_PREKEY_DICT);
    let mut session_store = PddbSessionStore::new(pddb_ses, SESSION_DICT);
    let mut rng = rand::rngs::OsRng.unwrap_err();

    // Sealed sender: sender identity is encrypted inside the ciphertext.
    // Decrypt to USMC first to reveal the actual sender, then decrypt inner message.
    if env_type == ENVELOPE_UNIDENTIFIED_SENDER {
        let usmc = match block_on(sealed_sender_decrypt_to_usmc(
            content.as_ref(),
            &identity_store,
        )) {
            Ok(u) => u,
            Err(e) => {
                log::warn!("main_ws: sealed_sender_decrypt_to_usmc failed: {e:?}");
                return;
            }
        };

        let trust_roots: Vec<PublicKey> = SEALED_SENDER_TRUST_ROOTS
            .iter()
            .filter_map(|b64| BASE64.decode(b64).ok())
            .filter_map(|raw| PublicKey::deserialize(&raw).ok())
            .collect();

        let sender_cert = match usmc.sender() {
            Ok(c) => c,
            Err(e) => { log::warn!("main_ws: usmc.sender() failed: {e:?}"); return; }
        };
        match sender_cert.validate_with_trust_roots(&trust_roots, Timestamp::from_epoch_millis(ts)) {
            Ok(true) => {}
            Ok(false) => { log::warn!("main_ws: sealed sender cert invalid — dropping"); return; }
            Err(e) => { log::warn!("main_ws: sealed sender cert error: {e:?}"); return; }
        }

        let sender_uuid = match sender_cert.sender_uuid() {
            Ok(u) => u.to_string(),
            Err(e) => { log::warn!("main_ws: sender_uuid: {e:?}"); return; }
        };
        let sender_dev_id = match sender_cert.sender_device_id() {
            Ok(d) => d,
            Err(e) => { log::warn!("main_ws: sender_device_id: {e:?}"); return; }
        };
        let remote_addr = ProtocolAddress::new(sender_uuid, sender_dev_id);

        let plaintext = match usmc.msg_type() {
            Ok(CiphertextMessageType::PreKey) => {
                let msg = match PreKeySignalMessage::try_from(usmc.contents().unwrap_or(&[])) {
                    Ok(m) => m,
                    Err(e) => { log::warn!("main_ws: SS PreKey parse: {e:?}"); return; }
                };
                match block_on(message_decrypt_prekey(
                    &msg, &remote_addr, local_addr,
                    &mut session_store, &mut identity_store,
                    &mut pre_key_store, &signed_pre_key_store,
                    &mut kyber_pre_key_store, &mut rng,
                )) {
                    Ok(pt) => { log::info!("main_ws: SS PREKEY decrypted {} bytes from {}", pt.len(), remote_addr.name()); pt }
                    Err(e) => { log::warn!("main_ws: SS PREKEY decrypt failed from {}: {e:?}", remote_addr.name()); return; }
                }
            }
            Ok(CiphertextMessageType::Whisper) => {
                let msg = match SignalMessage::try_from(usmc.contents().unwrap_or(&[])) {
                    Ok(m) => m,
                    Err(e) => { log::warn!("main_ws: SS Whisper parse: {e:?}"); return; }
                };
                match block_on(message_decrypt_signal(
                    &msg, &remote_addr,
                    &mut session_store, &mut identity_store, &mut rng,
                )) {
                    Ok(pt) => { log::info!("main_ws: SS CIPHERTEXT decrypted {} bytes from {}", pt.len(), remote_addr.name()); pt }
                    Err(e) => { log::warn!("main_ws: SS CIPHERTEXT decrypt failed from {}: {e:?}", remote_addr.name()); return; }
                }
            }
            Ok(other) => {
                log::warn!("main_ws: SS inner msg_type {:?} not supported — dropping", other);
                return;
            }
            Err(e) => { log::warn!("main_ws: SS msg_type: {e:?}"); return; }
        };

        deliver_content(plaintext, &remote_addr, ts, chat_cid);
        return;
    }

    // Non-sealed-sender: sender is in the outer envelope.
    if source_dev == 0 || source_dev > 127 {
        log::warn!("main_ws: source_device {source_dev} out of range (1..=127), skipping");
        return;
    }
    let sender_device = match DeviceId::new(source_dev as u8) {
        Ok(d) => d,
        Err(_) => return,
    };
    let remote_addr = ProtocolAddress::new(source_id, sender_device);

    let plaintext = match env_type {
        ENVELOPE_PREKEY_BUNDLE => {
            let msg = match PreKeySignalMessage::try_from(content.as_ref()) {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("main_ws: PreKeySignalMessage parse failed: {e:?}");
                    return;
                }
            };
            match block_on(message_decrypt_prekey(
                &msg,
                &remote_addr,
                local_addr,
                &mut session_store,
                &mut identity_store,
                &mut pre_key_store,
                &signed_pre_key_store,
                &mut kyber_pre_key_store,
                &mut rng,
            )) {
                Ok(pt) => {
                    log::info!("main_ws: PREKEY_BUNDLE decrypted {} bytes from {}",
                        pt.len(), remote_addr.name());
                    // TODO: prekey replenishment. Each successful PREKEY_BUNDLE decrypt consumes
                    // one of our uploaded one-time EC pre-keys on the server. sigchat does not
                    // currently upload replacements (no PUT /v2/keys flow), so eventually the
                    // server's stock runs out and new contacts can no longer establish sessions.
                    // See REPORTS/STATUS.md "prekey-replenishment" for tracking.
                    pt
                }
                Err(e) => {
                    log::warn!("main_ws: PREKEY_BUNDLE decrypt failed from {}: {e:?}",
                        remote_addr.name());
                    return;
                }
            }
        }
        ENVELOPE_CIPHERTEXT => {
            let msg = match SignalMessage::try_from(content.as_ref()) {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("main_ws: SignalMessage parse failed: {e:?}");
                    return;
                }
            };
            match block_on(message_decrypt_signal(
                &msg,
                &remote_addr,
                &mut session_store,
                &mut identity_store,
                &mut rng,
            )) {
                Ok(pt) => {
                    log::info!("main_ws: CIPHERTEXT decrypted {} bytes from {}",
                        pt.len(), remote_addr.name());
                    pt
                }
                Err(e) => {
                    log::warn!("main_ws: CIPHERTEXT decrypt failed from {}: {e:?}",
                        remote_addr.name());
                    return;
                }
            }
        }
        other => {
            log::warn!("main_ws: dropped envelope type={other} from {} (no dispatcher for this type)", remote_addr.name());
            return;
        }
    };

    deliver_content(plaintext, &remote_addr, ts, chat_cid);
}

// ---- Content delivery -------------------------------------------------------

/// Strip Signal's application-level padding: content_bytes + 0x80 + 0x00*N.
/// libsignal-protocol strips AES-CBC crypto padding but not this layer.
fn strip_signal_padding(mut plaintext: Vec<u8>) -> Vec<u8> {
    while plaintext.last() == Some(&0x00) { plaintext.pop(); }
    if plaintext.last() == Some(&0x80) { plaintext.pop(); }
    plaintext
}

fn deliver_content(plaintext: Vec<u8>, remote_addr: &ProtocolAddress, server_ts: u64, chat_cid: CID) {
    let plaintext = strip_signal_padding(plaintext);
    let content = match ContentProto::decode(plaintext.as_slice()) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("main_ws: Content proto decode failed from {}: {e}", remote_addr.name());
            return;
        }
    };

    let delivered = if let Some(dm) = content.data_message {
        let was_delivered = deliver_data_message(dm, remote_addr.name(), server_ts, chat_cid);
        if was_delivered {
            // Persist this peer as the V1 default outgoing recipient so that
            // SigChat::post() has somewhere to send a reply.
            if let Err(e) = crate::manager::outgoing::set_current_recipient(remote_addr) {
                log::warn!("main_ws: set_current_recipient failed: {e}");
            }
        }
        was_delivered
    } else if let Some(sync) = content.sync_message {
        deliver_sync_message(sync, server_ts, chat_cid)
    } else {
        log::warn!("main_ws: Content from {} has no DataMessage or SyncMessage — dropping", remote_addr.name());
        false
    };

    if delivered {
        chat::cf_redraw(chat_cid);
    }
}

fn deliver_data_message(dm: DataMessageProto, author: &str, server_ts: u64, chat_cid: CID) -> bool {
    let body = dm.body.unwrap_or_default();
    if body.is_empty() {
        log::warn!("main_ws: DataMessage with no body from {author} (attachment/reaction?) — not delivered to UI");
        return false;
    }
    let ts = dm.timestamp.unwrap_or(server_ts);
    chat::cf_post_add(chat_cid, author, ts, &body);
    log::info!("main_ws: delivered {} chars from {author}", body.len());
    true
}

fn deliver_sync_message(sync: SyncMessageProto, server_ts: u64, chat_cid: CID) -> bool {
    let sent = match sync.sent {
        Some(s) => s,
        None => {
            log::warn!("main_ws: SyncMessage has no Sent sub-message (contacts/request/etc.) — not delivered");
            return false;
        }
    };
    let sent_ts = sent.timestamp;
    let dest = sent.destination_service_id.unwrap_or_default();
    let dm = match sent.message {
        Some(m) => m,
        None => {
            log::warn!("main_ws: SyncMessage.Sent has no DataMessage — not delivered");
            return false;
        }
    };
    let body = dm.body.unwrap_or_default();
    if body.is_empty() {
        return false;
    }
    let ts = dm.timestamp.unwrap_or_else(|| sent_ts.unwrap_or(server_ts));
    // Prefix "→" marks messages sent by this device to distinguish from received.
    let author = format!("\u{2192}{}", &dest[..dest.len().min(8)]);
    chat::cf_post_add(chat_cid, &author, ts, &body);
    log::info!("main_ws: delivered {} chars (sync-sent to {})", body.len(), dest);
    true
}

fn is_timeout(e: &tungstenite::Error) -> bool {
    if let tungstenite::Error::Io(io_err) = e {
        matches!(io_err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
    } else {
        false
    }
}
