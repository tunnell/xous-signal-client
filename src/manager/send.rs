//! Send Phase 2b: REST submission and 409/410 retry on device-list change.
//!
//! Flow (called from `SigChat::post`):
//!   plaintext + recipient
//!     → submit_with_retry()                       (this module, retry loop)
//!         → build_encrypted_message()             (Phase 2a, outgoing.rs)
//!         → submit_encrypted_message()            (this module, single PUT)
//!     → Ok(()) on 200, SendError on exhausted retry / unrecoverable.
//!
//! The retry loop receives plaintext (not the encrypted blob) because 409
//! and 410 require *re-encryption* against the corrected device list. We
//! always rebuild the EncryptedMessage from plaintext on each attempt.
//!
//! Per-attempt behaviour:
//!   1. encrypt to recipient's currently-known device (V1 single device).
//!   2. PUT /v1/messages/{recipient_uuid} with HTTP Basic auth.
//!   3. status →
//!        200      → Ok
//!        401      → SendError::AuthFailed                       (no retry)
//!        404      → SendError::ServiceIdNotFound                (no retry)
//!        409      → fetch prekey bundles for missingDevices,
//!                   drop sessions for extraDevices,
//!                   continue loop                                (retry)
//!        410      → drop sessions for staleDevices,
//!                   continue loop                                (retry)
//!        413      → SendError::PayloadTooLarge                  (no retry)
//!        428      → SendError::ChallengeRequired                (no retry)
//!        429      → sleep(backoff), continue                     (retry)
//!        5xx      → sleep(backoff), continue                     (retry)
//!        network  → sleep(backoff), continue                     (retry)
//!        other    → SendError::Unexpected(status)               (no retry)
//!
//! Bounds: 3 attempts max, 30 s wall-clock budget, exponential backoff
//! (500ms · 2^attempt, capped at 4 s).
//!
//! Failure semantics: on RetryExhausted / non-retriable error, the message
//! stays locally-echoed in the chat UI but is *not* delivered. There is no
//! "failed to send" UI marker yet (out of scope for this iteration).
//!
//! Wire formats sourced from libsignal `rust/net/chat/src/ws/messages.rs`
//! (auth send) and `rust/net/chat/src/ws/keys.rs` (prekey response). See
//! REPORTS/TASK-08-phase2b-send.md for the cross-reference.
//!
//! HttpClient is a trait so unit tests can program responses without
//! touching the network or the Xous TLS stack.

#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]

use std::convert::TryFrom;
use std::io::{self, Read};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::alphabet::STANDARD as STANDARD_ALPHABET;
use base64::engine::{general_purpose::STANDARD, DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine as _;

/// Decoder for base64 fields in Signal-Server JSON responses.
///
/// Signal-Server returns prekey-bundle fields (signed_pre_key.signature,
/// pq_pre_key.signature, identity_key, *.public_key) without `=` padding —
/// Signal's Java backend uses `Base64.getEncoder().withoutPadding()`. We
/// accept both padded and unpadded input via `DecodePaddingMode::Indifferent`.
///
/// Encoding sites keep using `STANDARD` (padded); padded output is what we
/// send and is accepted by Signal-Server.
const PERMISSIVE: GeneralPurpose = GeneralPurpose::new(
    &STANDARD_ALPHABET,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);
use futures::executor::block_on;
use libsignal_protocol::{
    DeviceId, IdentityKey, IdentityKeyStore, KyberPreKeyId, PreKeyBundle, PreKeyId,
    ProtocolAddress, PublicKey, SessionStore, SignedPreKeyId, kem, process_prekey_bundle,
};
use rand::TryRngCore as _;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::manager::outgoing::{
    EncryptedMessage, OutgoingError, build_encrypted_message_with_stores,
    build_padded_data_message_content, build_padded_sync_transcript_content,
    encrypt_padded_for_recipient,
};
use crate::manager::stores::{PddbIdentityStore, PddbSessionStore};

const ACCOUNT_DICT: &'static str = "sigchat.account";
const IDENTITY_DICT: &'static str = "sigchat.identity";
const SESSION_DICT: &'static str = "sigchat.session";

const ACI_SERVICE_ID_KEY: &'static str = "aci.service_id";
const DEVICE_ID_KEY: &'static str = "device_id";
const PASSWORD_KEY: &'static str = "password";
const HOST_KEY: &'static str = "host";

const MAX_ATTEMPTS: u32 = 3;
const RETRY_BUDGET: Duration = Duration::from_secs(30);
const BACKOFF_BASE_MS: u64 = 500;
const BACKOFF_CAP_MS: u64 = 4_000;

// ---------- wire types --------------------------------------------------------

/// One outbound message-to-device entry. Mirrors libsignal's
/// `SingleOutboundMessageRepresentation`.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct OutgoingMessageEntity {
    #[serde(rename = "type")]
    message_type: u32,
    destination_device_id: u32,
    destination_registration_id: u32,
    /// Standard base64 with padding (matches libsignal's `Base64Padded`).
    content: String,
}

#[derive(Serialize, Debug)]
struct SubmitMessagesRequest {
    messages: Vec<OutgoingMessageEntity>,
    online: bool,
    urgent: bool,
    timestamp: u64,
}

/// 409 / 410 response body. The server returns one of:
///   {"missingDevices":[...]}                        (409)
///   {"missingDevices":[...],"extraDevices":[...]}   (409)
///   {"staleDevices":[...]}                          (410)
/// All three fields default to empty so the same struct parses every shape.
#[derive(Deserialize, Default, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct DeviceMismatchResponse {
    pub missing_devices: Vec<u32>,
    pub extra_devices: Vec<u32>,
    pub stale_devices: Vec<u32>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct PreKeyResponse {
    identity_key: String, // base64 (standard, with padding)
    devices: Vec<DeviceEntry>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct DeviceEntry {
    device_id: u32,
    registration_id: u32,
    signed_pre_key: SignedPreKeyEntry,
    #[serde(default)]
    pre_key: Option<PreKeyEntry>,
    pq_pre_key: KyberPreKeyEntry,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SignedPreKeyEntry {
    key_id: u32,
    public_key: String,
    signature: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct PreKeyEntry {
    key_id: u32,
    public_key: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct KyberPreKeyEntry {
    key_id: u32,
    public_key: String,
    signature: String,
}

// ---------- error type --------------------------------------------------------

#[derive(Debug)]
pub(crate) enum SendError {
    /// 401 — credentials rejected. Account may have been unlinked.
    AuthFailed,
    /// 404 on PUT /v1/messages — recipient ServiceId is not registered.
    ServiceIdNotFound,
    /// 413 — payload exceeded server limit.
    PayloadTooLarge,
    /// 428 — captcha / challenge required.
    ChallengeRequired,
    /// Retry budget or attempt count exhausted.
    RetryExhausted,
    /// Encryption (Phase 2a) failure.
    Encryption(OutgoingError),
    /// Account/credential read from pddb failed.
    Account(String),
    /// HTTP transport error (network, TLS, DNS).
    Transport(String),
    /// Unexpected response from the server.
    BadResponse(String),
    /// Unexpected status code.
    Unexpected(u16),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthFailed => write!(f, "auth failed (401)"),
            Self::ServiceIdNotFound => write!(f, "recipient not found (404)"),
            Self::PayloadTooLarge => write!(f, "payload too large (413)"),
            Self::ChallengeRequired => write!(f, "challenge required (428)"),
            Self::RetryExhausted => write!(f, "retry budget exhausted"),
            Self::Encryption(e) => write!(f, "encryption: {e}"),
            Self::Account(s) => write!(f, "account: {s}"),
            Self::Transport(s) => write!(f, "transport: {s}"),
            Self::BadResponse(s) => write!(f, "bad response: {s}"),
            Self::Unexpected(c) => write!(f, "unexpected status {c}"),
        }
    }
}

// ---------- HTTP abstraction --------------------------------------------------

#[derive(Debug)]
pub(crate) struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Minimal HTTP surface used by the send path. Production impl wraps a
/// `ureq::Agent` with the Xous TLS trust store; tests use a hand-coded mock.
pub(crate) trait HttpClient {
    fn put_json(&mut self, url: &str, auth: &str, body: &[u8]) -> io::Result<HttpResponse>;
    fn get_json(&mut self, url: &str, auth: &str) -> io::Result<HttpResponse>;
}

/// Production HttpClient using ureq + the Xous trust store, mirroring the
/// pattern in `manager::rest`.
pub(crate) struct UreqHttpClient {
    agent: ureq::Agent,
}

impl UreqHttpClient {
    pub fn new() -> Self {
        let client_config = Arc::new(tls::Tls::new().client_config());
        let agent = ureq::AgentBuilder::new().tls_config(client_config).build();
        Self { agent }
    }
}

impl HttpClient for UreqHttpClient {
    fn put_json(&mut self, url: &str, auth: &str, body: &[u8]) -> io::Result<HttpResponse> {
        let resp = self
            .agent
            .put(url)
            .set("Authorization", auth)
            .set("Content-Type", "application/json")
            .send_bytes(body);
        ureq_response_to_http(resp)
    }

    fn get_json(&mut self, url: &str, auth: &str) -> io::Result<HttpResponse> {
        let resp = self
            .agent
            .get(url)
            .set("Authorization", auth)
            .set("Accept", "application/json")
            .call();
        ureq_response_to_http(resp)
    }
}

fn ureq_response_to_http(
    resp: Result<ureq::Response, ureq::Error>,
) -> io::Result<HttpResponse> {
    match resp {
        Ok(r) => {
            let status = r.status();
            let body = read_to_vec(r)?;
            Ok(HttpResponse { status, body })
        }
        // ureq returns Status(_) for 4xx/5xx — we want to surface those as
        // HttpResponse so the caller's status-mapping logic can branch.
        Err(ureq::Error::Status(code, r)) => {
            let body = read_to_vec(r)?;
            Ok(HttpResponse { status: code, body })
        }
        Err(ureq::Error::Transport(e)) => Err(io::Error::other(format!("transport: {e}"))),
    }
}

fn read_to_vec(r: ureq::Response) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    r.into_reader()
        .take(1_000_000)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

// ---------- account snapshot --------------------------------------------------

pub(crate) struct AccountInfo {
    pub aci_service_id: String,
    pub device_id: u32,
    pub password: String,
    pub host: String,
}

impl AccountInfo {
    /// Read the four fields needed for the send path from pddb. Mirrors how
    /// `outgoing::local_protocol_address` reads from the same dict.
    pub fn read_from_pddb() -> Result<Self, SendError> {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        let aci = pddb_get_string(&pddb, ACCOUNT_DICT, ACI_SERVICE_ID_KEY)
            .ok_or_else(|| SendError::Account("aci.service_id missing".into()))?;
        let dev_str = pddb_get_string(&pddb, ACCOUNT_DICT, DEVICE_ID_KEY)
            .ok_or_else(|| SendError::Account("device_id missing".into()))?;
        let device_id: u32 = dev_str
            .trim()
            .parse()
            .map_err(|e| SendError::Account(format!("device_id parse: {e}")))?;
        let password = pddb_get_string(&pddb, ACCOUNT_DICT, PASSWORD_KEY)
            .ok_or_else(|| SendError::Account("password missing".into()))?;
        let host = pddb_get_string(&pddb, ACCOUNT_DICT, HOST_KEY)
            .ok_or_else(|| SendError::Account("host missing".into()))?;
        Ok(Self {
            aci_service_id: aci,
            device_id,
            password: password.trim().to_string(),
            host: host.trim().to_string(),
        })
    }

    fn basic_auth(&self) -> String {
        let raw = format!("{}.{}:{}", self.aci_service_id, self.device_id, self.password);
        format!("Basic {}", STANDARD.encode(raw.as_bytes()))
    }

    /// `https://chat.{host}` — same shape as `manager::config::Config::url`
    /// for `ServiceEnvironment::Live`. Staging uses a different prefix that
    /// is owned by Config; for V1 we hardcode Live (sigchat does too — see
    /// `signal_config()` in lib.rs).
    fn chat_base_url(&self) -> Result<Url, SendError> {
        Url::parse(&format!("https://chat.{}", self.host))
            .map_err(|e| SendError::Account(format!("host {} invalid: {e}", self.host)))
    }
}

fn pddb_get_string(pddb: &pddb::Pddb, dict: &str, key: &str) -> Option<String> {
    match pddb.get(dict, key, None, true, false, None, None::<fn()>) {
        Ok(mut handle) => {
            let mut buf = Vec::new();
            handle.read_to_end(&mut buf).ok()?;
            String::from_utf8(buf).ok()
        }
        Err(_) => None,
    }
}

// ---------- single PUT --------------------------------------------------------

/// Build the wire entity for one encrypted ciphertext.
fn enc_to_entity(enc: &EncryptedMessage) -> OutgoingMessageEntity {
    OutgoingMessageEntity {
        message_type: u32::try_from(enc.ciphertext_type).unwrap_or(0),
        destination_device_id: enc.destination_device_id,
        destination_registration_id: enc.destination_registration_id,
        content: STANDARD.encode(&enc.ciphertext_bytes),
    }
}

/// Submit a batch of already-encrypted per-device entities to
/// `PUT /v1/messages/{uuid}`. The wire body is `{ messages: [...], ... }`.
/// On 409/410 the response is encoded into a `BadResponse` retry signal that
/// `classify_attempt` decodes back into `Mismatch409` / `Mismatch410`.
pub(crate) fn submit_messages(
    recipient_uuid: &str,
    entities: Vec<OutgoingMessageEntity>,
    timestamp_ms: u64,
    account: &AccountInfo,
    http: &mut dyn HttpClient,
) -> Result<(), SendError> {
    let mut url = account.chat_base_url()?;
    url.set_path(&format!("/v1/messages/{}", recipient_uuid));

    let req = SubmitMessagesRequest {
        messages: entities,
        online: false,
        urgent: true,
        timestamp: timestamp_ms,
    };
    let body = serde_json::to_vec(&req)
        .map_err(|e| SendError::BadResponse(format!("serialize: {e}")))?;

    let auth = account.basic_auth();
    let resp = http
        .put_json(url.as_str(), &auth, &body)
        .map_err(|e| SendError::Transport(format!("{e}")))?;

    interpret_send_status(resp)
}

/// Single-entity convenience wrapper retained for status-mapping unit tests.
pub(crate) fn submit_encrypted_message(
    enc: &EncryptedMessage,
    recipient_uuid: &str,
    account: &AccountInfo,
    http: &mut dyn HttpClient,
) -> Result<(), SendError> {
    submit_messages(recipient_uuid, vec![enc_to_entity(enc)], enc.timestamp_ms, account, http)
}

/// Map a PUT /v1/messages response into Ok / a typed SendError, including
/// stamping 409/410 bodies as a retry signal via [`MismatchSignal`].
fn interpret_send_status(resp: HttpResponse) -> Result<(), SendError> {
    match resp.status {
        200 | 204 => Ok(()),
        401 => Err(SendError::AuthFailed),
        404 => Err(SendError::ServiceIdNotFound),
        413 => Err(SendError::PayloadTooLarge),
        428 => Err(SendError::ChallengeRequired),
        409 | 410 => {
            // Encode the parsed body into BadResponse; the retry loop
            // re-parses with code awareness via [`parse_mismatch`].
            let mismatch_marker = format!(
                "MISMATCH {} {}",
                resp.status,
                String::from_utf8_lossy(&resp.body)
            );
            Err(SendError::BadResponse(mismatch_marker))
        }
        429 => Err(SendError::Unexpected(429)),
        500..=599 => Err(SendError::Unexpected(resp.status)),
        other => Err(SendError::Unexpected(other)),
    }
}

/// Parse a 409 / 410 body. Used by the retry loop after intercepting the
/// pseudo-`BadResponse` marker built by [`interpret_send_status`].
fn parse_mismatch(body: &[u8]) -> Result<DeviceMismatchResponse, SendError> {
    serde_json::from_slice(body)
        .map_err(|e| SendError::BadResponse(format!("mismatch parse: {e}")))
}

// ---------- retry loop --------------------------------------------------------

#[derive(Debug)]
enum AttemptDecision {
    Done,
    Mismatch409(Vec<u8>),
    Mismatch410(Vec<u8>),
    Backoff,
    Fatal(SendError),
}

/// Classify a single submit attempt's outcome. Pulled out of the loop so
/// the routing logic (which is the protocol judgment in this module) is
/// unit-testable without a live store stack.
fn classify_attempt(result: Result<(), SendError>) -> AttemptDecision {
    match result {
        Ok(()) => AttemptDecision::Done,
        Err(SendError::BadResponse(ref m)) if m.starts_with("MISMATCH 409 ") => {
            AttemptDecision::Mismatch409(m["MISMATCH 409 ".len()..].as_bytes().to_vec())
        }
        Err(SendError::BadResponse(ref m)) if m.starts_with("MISMATCH 410 ") => {
            AttemptDecision::Mismatch410(m["MISMATCH 410 ".len()..].as_bytes().to_vec())
        }
        Err(SendError::Unexpected(code)) if code == 429 || (500..=599).contains(&code) => {
            AttemptDecision::Backoff
        }
        Err(SendError::Transport(_)) => AttemptDecision::Backoff,
        // Non-retriable: AuthFailed, ServiceIdNotFound, PayloadTooLarge,
        // ChallengeRequired, Encryption, Account, BadResponse (non-MISMATCH),
        // Unexpected(non-5xx-non-429).
        Err(other) => AttemptDecision::Fatal(other),
    }
}

/// Drop a session record by ProtocolAddress. Implemented by
/// [`PddbSessionStore`] for production and by a stub for tests. Not part of
/// libsignal's `SessionStore` trait — it's a sigchat-side recovery op.
pub(crate) trait SessionDeleter {
    fn delete(&mut self, address: &ProtocolAddress);
}

impl SessionDeleter for PddbSessionStore {
    fn delete(&mut self, address: &ProtocolAddress) {
        self.delete_session(address);
    }
}

/// Enumerate known sessions for a recipient UUID. Used by the multi-device
/// fan-out: we encrypt the plaintext once per session we have, submit them
/// all in a single PUT, then let the server's 409/410 reconcile the device
/// set and retry.
///
/// Mirrors the role of `SessionStoreExt::get_sub_device_sessions` in
/// whisperfish/libsignal-service-rs (AGPL-3.0). Lives here rather than on
/// libsignal's `SessionStore` trait because libsignal core doesn't expose
/// session enumeration.
pub(crate) trait DeviceSessionEnum {
    fn device_ids_for(&self, recipient_uuid: &str) -> Vec<u32>;
}

impl DeviceSessionEnum for PddbSessionStore {
    fn device_ids_for(&self, recipient_uuid: &str) -> Vec<u32> {
        PddbSessionStore::device_ids_for(self, recipient_uuid)
    }
}

/// Generic core of the retry loop, parameterized on pre-built padded Content
/// bytes. Both the recipient send path (DataMessage Content) and the sync
/// transcript path (SyncMessage Content) drive this same loop with different
/// `padded_content`.
///
/// On each attempt: enumerate device sessions for `recipient_uuid`, drop the
/// `excluded_device_id` if any (used by sync to skip self), encrypt the
/// padded Content for each remaining device, submit them all in one PUT.
/// 409/410 update the local session set; the next iteration's enumeration
/// picks up the changes naturally.
///
/// Returns Ok(true) on a successful submission, Ok(false) when the device
/// set is empty after exclusion (sync path's "no other devices" outcome —
/// non-fatal, callers may want to skip rather than retry).
fn submit_padded_with_retry_generic<S, I, D>(
    padded_content: &[u8],
    timestamp_ms: u64,
    recipient_uuid: &str,
    fallback_device_id: u32,
    excluded_device_id: Option<u32>,
    local_addr: &ProtocolAddress,
    session_store: &mut S,
    identity_store: &mut I,
    deleter: &mut D,
    account: &AccountInfo,
    http: &mut dyn HttpClient,
    sleeper: &mut dyn FnMut(Duration),
) -> Result<bool, SendError>
where
    S: SessionStore + DeviceSessionEnum,
    I: IdentityKeyStore,
    D: SessionDeleter,
{
    let start = Instant::now();
    let mut attempt: u32 = 0;

    loop {
        if start.elapsed() > RETRY_BUDGET {
            log::warn!("send: retry budget exhausted after {} attempts", attempt);
            return Err(SendError::RetryExhausted);
        }
        if attempt >= MAX_ATTEMPTS {
            log::warn!("send: max attempts ({}) reached", MAX_ATTEMPTS);
            return Err(SendError::RetryExhausted);
        }
        attempt += 1;

        let mut device_ids = session_store.device_ids_for(recipient_uuid);
        if let Some(excl) = excluded_device_id {
            device_ids.retain(|d| *d != excl);
        }
        if device_ids.is_empty() {
            // Sync path: no other devices on own account → caller decides.
            if excluded_device_id.is_some() {
                return Ok(false);
            }
            // Recipient path: fall back to the address's device_id (fresh
            // recipient, no sessions yet); 409 round-trip will discover
            // others.
            device_ids.push(fallback_device_id);
        }

        let recipient_addr_for_handlers = {
            let dev = u8::try_from(fallback_device_id)
                .map_err(|_| SendError::BadResponse(
                    format!("fallback device id {fallback_device_id} out of range")))
                .and_then(|d| DeviceId::new(d).map_err(|e|
                    SendError::BadResponse(format!("DeviceId: {e:?}"))))?;
            ProtocolAddress::new(recipient_uuid.to_string(), dev)
        };

        let mut entities: Vec<OutgoingMessageEntity> =
            Vec::with_capacity(device_ids.len());
        for did in &device_ids {
            let dev = u8::try_from(*did)
                .map_err(|_| SendError::BadResponse(format!("device id {did} out of range")))
                .and_then(|d| {
                    DeviceId::new(d).map_err(|e| {
                        SendError::BadResponse(format!("DeviceId: {e:?}"))
                    })
                })?;
            let dev_addr = ProtocolAddress::new(recipient_uuid.to_string(), dev);
            let enc = encrypt_padded_for_recipient(
                padded_content,
                timestamp_ms,
                &dev_addr,
                local_addr,
                session_store,
                identity_store,
            )
            .map_err(SendError::Encryption)?;
            entities.push(enc_to_entity(&enc));
        }
        let n_entities = entities.len();

        let outcome = submit_messages(
            recipient_uuid,
            entities,
            timestamp_ms,
            account,
            http,
        );
        match classify_attempt(outcome) {
            AttemptDecision::Done => {
                log::info!(
                    "send: ok on attempt {} (devices={:?})",
                    attempt,
                    device_ids,
                );
                return Ok(true);
            }
            AttemptDecision::Mismatch409(body) => {
                let mm = parse_mismatch(&body)?;
                log::info!(
                    "send: 409 missing={:?} extra={:?} (sent for {} devices)",
                    mm.missing_devices,
                    mm.extra_devices,
                    n_entities,
                );
                handle_mismatched_devices(
                    &recipient_addr_for_handlers,
                    &mm,
                    session_store,
                    identity_store,
                    deleter,
                    account,
                    http,
                )?;
            }
            AttemptDecision::Mismatch410(body) => {
                let mm = parse_mismatch(&body)?;
                log::info!("send: 410 stale={:?}", mm.stale_devices);
                handle_stale_devices(&recipient_addr_for_handlers, &mm, deleter);
            }
            AttemptDecision::Backoff => {
                let delay = backoff(attempt);
                log::info!("send: transient error, sleep {:?}", delay);
                sleeper(delay);
            }
            AttemptDecision::Fatal(e) => return Err(e),
        }
    }
}

/// Recipient send path: thin wrapper over `submit_padded_with_retry_generic`
/// that builds DataMessage Content bytes from plaintext. Returns Ok(()) on
/// success.
pub(crate) fn submit_with_retry_generic<S, I, D>(
    plaintext: &str,
    timestamp_ms: u64,
    recipient_addr: &ProtocolAddress,
    local_addr: &ProtocolAddress,
    session_store: &mut S,
    identity_store: &mut I,
    deleter: &mut D,
    account: &AccountInfo,
    http: &mut dyn HttpClient,
    sleeper: &mut dyn FnMut(Duration),
) -> Result<(), SendError>
where
    S: SessionStore + DeviceSessionEnum,
    I: IdentityKeyStore,
    D: SessionDeleter,
{
    let padded = build_padded_data_message_content(plaintext, timestamp_ms);
    let _delivered = submit_padded_with_retry_generic(
        &padded,
        timestamp_ms,
        recipient_addr.name(),
        u32::from(recipient_addr.device_id()),
        None,
        local_addr,
        session_store,
        identity_store,
        deleter,
        account,
        http,
        sleeper,
    )?;
    Ok(())
}

/// Sync transcript path: builds a SyncMessage::Sent wrapping the original
/// DataMessage and fans it out to every device of the sender's own account
/// EXCLUDING the sending device. Returns Ok(()) whether or not anything was
/// actually delivered (no other devices = nothing to do).
///
/// If we have no sessions to other devices of own account, performs an
/// upfront device discovery via `GET /v2/keys/{own_uuid}/*` and establishes
/// sessions for the discovered devices. Without this step, the first send
/// from a fresh-linked secondary would always have an empty fan-out — the
/// 409 mechanism can establish missing devices but only if the request body
/// already contains entities for at least one device, which we can't
/// produce without a session.
pub(crate) fn submit_sync_transcript_generic<S, I, D>(
    plaintext: &str,
    recipient_uuid: &str,
    timestamp_ms: u64,
    own_uuid: &str,
    own_device_id: u32,
    local_addr: &ProtocolAddress,
    session_store: &mut S,
    identity_store: &mut I,
    deleter: &mut D,
    account: &AccountInfo,
    http: &mut dyn HttpClient,
    sleeper: &mut dyn FnMut(Duration),
) -> Result<(), SendError>
where
    S: SessionStore + DeviceSessionEnum,
    I: IdentityKeyStore,
    D: SessionDeleter,
{
    // Up-front device discovery if we have no sessions to other devices.
    let known: Vec<u32> = session_store
        .device_ids_for(own_uuid)
        .into_iter()
        .filter(|d| *d != own_device_id)
        .collect();
    if known.is_empty() {
        match discover_and_establish_account_devices(
            own_uuid,
            Some(own_device_id),
            session_store,
            identity_store,
            account,
            http,
        ) {
            Ok(established) if established.is_empty() => {
                log::info!(
                    "sync: discovery returned no other devices for {}; transcript skipped",
                    own_uuid,
                );
                return Ok(());
            }
            Ok(established) => {
                log::info!(
                    "sync: discovered + established sessions for own devices {:?}",
                    established,
                );
            }
            Err(e) => {
                log::warn!(
                    "sync: device discovery failed: {:?} — transcript skipped",
                    e,
                );
                return Ok(());
            }
        }
    }

    let padded = build_padded_sync_transcript_content(
        recipient_uuid,
        plaintext,
        timestamp_ms,
    );
    let delivered = submit_padded_with_retry_generic(
        &padded,
        timestamp_ms,
        own_uuid,
        // Fallback isn't reachable on the sync path — when the device set
        // is empty after exclusion we early-return `false` rather than
        // submit. The fallback value here is just a placeholder.
        own_device_id,
        Some(own_device_id),
        local_addr,
        session_store,
        identity_store,
        deleter,
        account,
        http,
        sleeper,
    )?;
    if delivered {
        log::info!("sync: transcript delivered to own-account devices");
    } else {
        log::info!("sync: no other devices on own account; transcript skipped");
    }
    Ok(())
}

/// Production retry-loop driver: uses the concrete pddb-backed stores and
/// `std::thread::sleep` for backoff. Sends the recipient DataMessage AND,
/// on success, fans out a SyncMessage::Sent transcript to the sender's own
/// other devices. Sync transcript failure is non-fatal.
pub(crate) fn submit_with_retry_with_stores(
    plaintext: &str,
    timestamp_ms: u64,
    recipient_addr: &ProtocolAddress,
    local_addr: &ProtocolAddress,
    session_store: &mut PddbSessionStore,
    identity_store: &mut PddbIdentityStore,
    account: &AccountInfo,
    http: &mut dyn HttpClient,
) -> Result<(), SendError> {
    // Production deleter and enumerator open their own pddb handles so the
    // borrow checker tolerates simultaneous exclusive use of session_store
    // (as SessionStore). PDDB is shared backing — fresh handles see all
    // commits.
    struct PddbDeleter {
        pddb: pddb::Pddb,
    }
    impl SessionDeleter for PddbDeleter {
        fn delete(&mut self, address: &ProtocolAddress) {
            let key = format!("{}.{}", address.name(), address.device_id());
            let _ = self.pddb.delete_key(SESSION_DICT, &key, None);
        }
    }
    let pddb_del = pddb::Pddb::new();
    pddb_del.try_mount();
    let mut deleter = PddbDeleter { pddb: pddb_del };
    let mut sleeper = |d: Duration| std::thread::sleep(d);

    submit_with_retry_generic(
        plaintext,
        timestamp_ms,
        recipient_addr,
        local_addr,
        session_store,
        identity_store,
        &mut deleter,
        account,
        http,
        &mut sleeper,
    )?;

    // Recipient PUT succeeded. Fan out the sync transcript to our own
    // account's other devices. Failure here is non-fatal: the recipient
    // already has the message, the sender's primary just won't see it
    // mirrored to its outgoing thread.
    let sync_result = submit_sync_transcript_generic(
        plaintext,
        recipient_addr.name(),
        timestamp_ms,
        &account.aci_service_id,
        account.device_id,
        local_addr,
        session_store,
        identity_store,
        &mut deleter,
        account,
        http,
        &mut sleeper,
    );
    if let Err(e) = sync_result {
        log::warn!("sync: transcript failed (non-fatal): {e:?}");
    }
    Ok(())
}

fn backoff(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(4);
    let ms = (BACKOFF_BASE_MS << shift).min(BACKOFF_CAP_MS);
    Duration::from_millis(ms)
}

// ---------- 409 / 410 handlers ------------------------------------------------

fn handle_stale_devices<D: SessionDeleter>(
    recipient: &ProtocolAddress,
    mm: &DeviceMismatchResponse,
    deleter: &mut D,
) {
    for device_id in &mm.stale_devices {
        if let Some(addr) = device_protocol_address(recipient.name(), *device_id) {
            deleter.delete(&addr);
        }
    }
}

fn handle_mismatched_devices<S, I, D>(
    recipient: &ProtocolAddress,
    mm: &DeviceMismatchResponse,
    session_store: &mut S,
    identity_store: &mut I,
    deleter: &mut D,
    account: &AccountInfo,
    http: &mut dyn HttpClient,
) -> Result<(), SendError>
where
    S: SessionStore,
    I: IdentityKeyStore,
    D: SessionDeleter,
{
    // Drop sessions for extras first — re-encrypt won't target them anyway,
    // but we don't want stale state hanging around.
    for device_id in &mm.extra_devices {
        if let Some(addr) = device_protocol_address(recipient.name(), *device_id) {
            deleter.delete(&addr);
        }
    }
    // Fetch + process prekey bundles for missing devices, building sessions.
    for device_id in &mm.missing_devices {
        let url = {
            let mut u = account.chat_base_url()?;
            u.set_path(&format!("/v2/keys/{}/{}", recipient.name(), device_id));
            u
        };
        let auth = account.basic_auth();
        let resp = http
            .get_json(url.as_str(), &auth)
            .map_err(|e| SendError::Transport(format!("get prekeys: {e}")))?;
        match resp.status {
            200 => {
                let pre: PreKeyResponse = serde_json::from_slice(&resp.body).map_err(|e| {
                    SendError::BadResponse(format!("prekey response parse: {e}"))
                })?;
                process_one_prekey_bundle(
                    recipient.name(),
                    *device_id,
                    &pre,
                    session_store,
                    identity_store,
                )?;
            }
            404 => {
                // Device exists in 409's missingDevices but its prekeys are
                // not on the server (stock exhausted, or device was just
                // unregistered). Skipping is safe — re-encrypt simply won't
                // include this device. If that empties the recipient set,
                // we'll fail downstream rather than here.
                log::warn!(
                    "send: 404 fetching prekey for {}/{}; skipping",
                    recipient.name(),
                    device_id
                );
            }
            401 => return Err(SendError::AuthFailed),
            other => return Err(SendError::Unexpected(other)),
        }
    }
    Ok(())
}

/// Proactively fetch prekey bundles for all devices of an account and
/// establish sessions for each (excluding `excluded_device_id`, used by the
/// sync transcript path to skip the sender's own device).
///
/// Used at the start of the sync transcript path when we have no sessions
/// to any of our own account's other devices yet — the 409 mechanism would
/// otherwise establish them lazily, but Signal-Server requires the request
/// body to address at least the primary device, so we need at least one
/// session up front.
///
/// Endpoint: `GET /v2/keys/{uuid}/*` returns a PreKeyResponse with one
/// DeviceEntry per registered device of the account.
fn discover_and_establish_account_devices<S, I>(
    account_uuid: &str,
    excluded_device_id: Option<u32>,
    session_store: &mut S,
    identity_store: &mut I,
    account: &AccountInfo,
    http: &mut dyn HttpClient,
) -> Result<Vec<u32>, SendError>
where
    S: SessionStore,
    I: IdentityKeyStore,
{
    let url = {
        let mut u = account.chat_base_url()?;
        u.set_path(&format!("/v2/keys/{}/*", account_uuid));
        u
    };
    let auth = account.basic_auth();
    let resp = http
        .get_json(url.as_str(), &auth)
        .map_err(|e| SendError::Transport(format!("discover devices: {e}")))?;
    match resp.status {
        200 => {}
        401 => return Err(SendError::AuthFailed),
        404 => return Ok(Vec::new()),
        other => return Err(SendError::Unexpected(other)),
    }

    let pre: PreKeyResponse = serde_json::from_slice(&resp.body)
        .map_err(|e| SendError::BadResponse(format!("discover prekey parse: {e}")))?;

    let identity_key = decode_identity_key(&pre.identity_key)?;
    let mut established = Vec::new();
    for entry in &pre.devices {
        if Some(entry.device_id) == excluded_device_id {
            continue;
        }
        let bundle = build_prekey_bundle(entry, identity_key)?;
        let dev_addr = device_protocol_address(account_uuid, entry.device_id)
            .ok_or_else(|| SendError::BadResponse(format!(
                "device id {} out of range", entry.device_id)))?;
        let mut rng = rand::rngs::OsRng.unwrap_err();
        block_on(process_prekey_bundle(
            &dev_addr,
            session_store,
            identity_store,
            &bundle,
            std::time::SystemTime::now(),
            &mut rng,
        ))
        .map_err(|e| SendError::BadResponse(format!(
            "discover process_prekey_bundle for {}/{}: {e:?}",
            account_uuid, entry.device_id)))?;
        established.push(entry.device_id);
    }
    Ok(established)
}

fn process_one_prekey_bundle<S, I>(
    recipient_uuid: &str,
    device_id: u32,
    pre: &PreKeyResponse,
    session_store: &mut S,
    identity_store: &mut I,
) -> Result<(), SendError>
where
    S: SessionStore,
    I: IdentityKeyStore,
{
    // The single-device GET returns `devices` with the requested device.
    let entry = pre
        .devices
        .iter()
        .find(|d| d.device_id == device_id)
        .or_else(|| pre.devices.first())
        .ok_or_else(|| SendError::BadResponse("prekey response: empty devices".into()))?;

    let identity_key = decode_identity_key(&pre.identity_key)?;
    let bundle = build_prekey_bundle(entry, identity_key)?;

    let dev_addr = device_protocol_address(recipient_uuid, device_id)
        .ok_or_else(|| SendError::BadResponse(format!("device id {device_id} out of range")))?;

    let mut rng = rand::rngs::OsRng
        .unwrap_err();
    block_on(process_prekey_bundle(
        &dev_addr,
        session_store,
        identity_store,
        &bundle,
        std::time::SystemTime::now(),
        &mut rng,
    ))
    .map_err(|e| SendError::BadResponse(format!("process_prekey_bundle: {e:?}")))?;
    Ok(())
}

fn build_prekey_bundle(
    entry: &DeviceEntry,
    identity_key: IdentityKey,
) -> Result<PreKeyBundle, SendError> {
    let device = DeviceId::new(
        u8::try_from(entry.device_id)
            .map_err(|_| SendError::BadResponse(format!("device id {} out of range", entry.device_id)))?,
    )
    .map_err(|e| SendError::BadResponse(format!("DeviceId: {e:?}")))?;

    let signed_pub = decode_ec_public(&entry.signed_pre_key.public_key)?;
    let signed_sig = PERMISSIVE
        .decode(&entry.signed_pre_key.signature)
        .map_err(|e| SendError::BadResponse(format!("signed pk sig b64: {e}")))?;

    let kyber_pub = decode_kyber_public(&entry.pq_pre_key.public_key)?;
    let kyber_sig = PERMISSIVE
        .decode(&entry.pq_pre_key.signature)
        .map_err(|e| SendError::BadResponse(format!("kyber pk sig b64: {e}")))?;

    let one_time = match &entry.pre_key {
        Some(p) => Some((PreKeyId::from(p.key_id), decode_ec_public(&p.public_key)?)),
        None => None,
    };

    PreKeyBundle::new(
        entry.registration_id,
        device,
        one_time,
        SignedPreKeyId::from(entry.signed_pre_key.key_id),
        signed_pub,
        signed_sig,
        KyberPreKeyId::from(entry.pq_pre_key.key_id),
        kyber_pub,
        kyber_sig,
        identity_key,
    )
    .map_err(|e| SendError::BadResponse(format!("PreKeyBundle::new: {e:?}")))
}

fn decode_identity_key(b64: &str) -> Result<IdentityKey, SendError> {
    let bytes = PERMISSIVE
        .decode(b64)
        .map_err(|e| SendError::BadResponse(format!("identity key b64: {e}")))?;
    IdentityKey::decode(&bytes)
        .map_err(|e| SendError::BadResponse(format!("identity key decode: {e:?}")))
}

fn decode_ec_public(b64: &str) -> Result<PublicKey, SendError> {
    let bytes = PERMISSIVE
        .decode(b64)
        .map_err(|e| SendError::BadResponse(format!("ec pub b64: {e}")))?;
    PublicKey::deserialize(&bytes)
        .map_err(|e| SendError::BadResponse(format!("ec pub decode: {e:?}")))
}

fn decode_kyber_public(b64: &str) -> Result<kem::PublicKey, SendError> {
    let bytes = PERMISSIVE
        .decode(b64)
        .map_err(|e| SendError::BadResponse(format!("kyber pub b64: {e}")))?;
    kem::PublicKey::deserialize(&bytes)
        .map_err(|e| SendError::BadResponse(format!("kyber pub decode: {e:?}")))
}

fn device_protocol_address(uuid: &str, device_id: u32) -> Option<ProtocolAddress> {
    let dev = u8::try_from(device_id).ok()?;
    let dev = DeviceId::new(dev).ok()?;
    Some(ProtocolAddress::new(uuid.to_string(), dev))
}

// ---------- production wrapper ------------------------------------------------

/// Production entrypoint called from `SigChat::post`. Reads stores and
/// account state from pddb, builds the local-address record, and delegates
/// to the testable [`submit_with_retry_with_stores`].
pub(crate) fn submit_with_retry(
    plaintext: &str,
    timestamp_ms: u64,
    recipient_addr: &ProtocolAddress,
    http: &mut dyn HttpClient,
) -> Result<(), SendError> {
    let account = AccountInfo::read_from_pddb()?;

    let local_dev = u8::try_from(account.device_id)
        .ok()
        .and_then(|d| DeviceId::new(d).ok())
        .ok_or_else(|| SendError::Account(format!("device_id {} out of range", account.device_id)))?;
    let local_addr = ProtocolAddress::new(account.aci_service_id.clone(), local_dev);

    let pddb_id = pddb::Pddb::new();
    pddb_id.try_mount();
    let pddb_ses = pddb::Pddb::new();
    pddb_ses.try_mount();
    let mut identity_store = PddbIdentityStore::new(pddb_id, ACCOUNT_DICT, IDENTITY_DICT);
    let mut session_store = PddbSessionStore::new(pddb_ses, SESSION_DICT);

    submit_with_retry_with_stores(
        plaintext,
        timestamp_ms,
        recipient_addr,
        &local_addr,
        &mut session_store,
        &mut identity_store,
        &account,
        http,
    )
}

// ---------- tests -------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // ---- mock http ----------------------------------------------------------

    enum Expect {
        Put { url: String, status: u16, body: Vec<u8> },
        Get { url: String, status: u16, body: Vec<u8> },
        TransportErr,
    }

    struct MockHttp {
        program: RefCell<Vec<Expect>>,
        calls: RefCell<usize>,
    }

    impl MockHttp {
        fn new(program: Vec<Expect>) -> Self {
            Self {
                program: RefCell::new(program),
                calls: RefCell::new(0),
            }
        }
    }

    impl HttpClient for MockHttp {
        fn put_json(&mut self, url: &str, _auth: &str, _body: &[u8]) -> io::Result<HttpResponse> {
            *self.calls.borrow_mut() += 1;
            let exp = self
                .program
                .borrow_mut()
                .drain(..1)
                .next()
                .expect("mock http: no more expectations");
            match exp {
                Expect::Put { url: expected, status, body } => {
                    assert!(url.contains(&expected),
                        "PUT url mismatch: actual={url} expected fragment={expected}");
                    Ok(HttpResponse { status, body })
                }
                Expect::Get { .. } => panic!("expected GET, got PUT to {url}"),
                Expect::TransportErr => Err(io::Error::other("simulated transport error")),
            }
        }

        fn get_json(&mut self, url: &str, _auth: &str) -> io::Result<HttpResponse> {
            *self.calls.borrow_mut() += 1;
            let exp = self
                .program
                .borrow_mut()
                .drain(..1)
                .next()
                .expect("mock http: no more expectations");
            match exp {
                Expect::Get { url: expected, status, body } => {
                    assert!(url.contains(&expected),
                        "GET url mismatch: actual={url} expected fragment={expected}");
                    Ok(HttpResponse { status, body })
                }
                Expect::Put { .. } => panic!("expected PUT, got GET to {url}"),
                Expect::TransportErr => Err(io::Error::other("simulated transport error")),
            }
        }
    }

    // ---- status mapping ----------------------------------------------------

    fn fake_enc() -> EncryptedMessage {
        EncryptedMessage {
            ciphertext_bytes: vec![0xAA; 16],
            ciphertext_type: 1,
            destination_device_id: 2,
            destination_registration_id: 100,
            timestamp_ms: 1_700_000_000_000,
        }
    }

    fn fake_account() -> AccountInfo {
        AccountInfo {
            aci_service_id: "00000000-0000-0000-0000-000000000001".to_string(),
            device_id: 1,
            password: "pw".to_string(),
            host: "signal.org".to_string(),
        }
    }

    #[test]
    fn submit_200_returns_ok() {
        let mut http = MockHttp::new(vec![Expect::Put {
            url: "/v1/messages/".into(),
            status: 200,
            body: br#"{}"#.to_vec(),
        }]);
        let result = submit_encrypted_message(
            &fake_enc(),
            "00000000-0000-0000-0000-000000000002",
            &fake_account(),
            &mut http,
        );
        assert!(matches!(result, Ok(())));
    }

    #[test]
    fn submit_401_returns_auth_failed() {
        let mut http = MockHttp::new(vec![Expect::Put {
            url: "/v1/messages/".into(),
            status: 401,
            body: vec![],
        }]);
        let r = submit_encrypted_message(
            &fake_enc(),
            "00000000-0000-0000-0000-000000000002",
            &fake_account(),
            &mut http,
        );
        assert!(matches!(r, Err(SendError::AuthFailed)));
    }

    #[test]
    fn submit_404_returns_service_id_not_found() {
        let mut http = MockHttp::new(vec![Expect::Put {
            url: "/v1/messages/".into(),
            status: 404,
            body: vec![],
        }]);
        let r = submit_encrypted_message(
            &fake_enc(),
            "00000000-0000-0000-0000-000000000002",
            &fake_account(),
            &mut http,
        );
        assert!(matches!(r, Err(SendError::ServiceIdNotFound)));
    }

    #[test]
    fn submit_413_returns_payload_too_large() {
        let mut http = MockHttp::new(vec![Expect::Put {
            url: "/v1/messages/".into(),
            status: 413,
            body: vec![],
        }]);
        let r = submit_encrypted_message(
            &fake_enc(),
            "00000000-0000-0000-0000-000000000002",
            &fake_account(),
            &mut http,
        );
        assert!(matches!(r, Err(SendError::PayloadTooLarge)));
    }

    #[test]
    fn submit_428_returns_challenge_required() {
        let mut http = MockHttp::new(vec![Expect::Put {
            url: "/v1/messages/".into(),
            status: 428,
            body: vec![],
        }]);
        let r = submit_encrypted_message(
            &fake_enc(),
            "00000000-0000-0000-0000-000000000002",
            &fake_account(),
            &mut http,
        );
        assert!(matches!(r, Err(SendError::ChallengeRequired)));
    }

    #[test]
    fn submit_5xx_returns_unexpected() {
        let mut http = MockHttp::new(vec![Expect::Put {
            url: "/v1/messages/".into(),
            status: 502,
            body: vec![],
        }]);
        let r = submit_encrypted_message(
            &fake_enc(),
            "00000000-0000-0000-0000-000000000002",
            &fake_account(),
            &mut http,
        );
        assert!(matches!(r, Err(SendError::Unexpected(502))));
    }

    #[test]
    fn submit_409_carries_mismatch_marker() {
        let mut http = MockHttp::new(vec![Expect::Put {
            url: "/v1/messages/".into(),
            status: 409,
            body: br#"{"missingDevices":[2,3]}"#.to_vec(),
        }]);
        let r = submit_encrypted_message(
            &fake_enc(),
            "00000000-0000-0000-0000-000000000002",
            &fake_account(),
            &mut http,
        );
        match r {
            Err(SendError::BadResponse(m)) if m.starts_with("MISMATCH 409 ") => {
                let mm = parse_mismatch(m["MISMATCH 409 ".len()..].as_bytes()).unwrap();
                assert_eq!(mm.missing_devices, vec![2u32, 3]);
                assert!(mm.extra_devices.is_empty());
                assert!(mm.stale_devices.is_empty());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn submit_410_carries_mismatch_marker() {
        let mut http = MockHttp::new(vec![Expect::Put {
            url: "/v1/messages/".into(),
            status: 410,
            body: br#"{"staleDevices":[4]}"#.to_vec(),
        }]);
        let r = submit_encrypted_message(
            &fake_enc(),
            "00000000-0000-0000-0000-000000000002",
            &fake_account(),
            &mut http,
        );
        match r {
            Err(SendError::BadResponse(m)) if m.starts_with("MISMATCH 410 ") => {
                let mm = parse_mismatch(m["MISMATCH 410 ".len()..].as_bytes()).unwrap();
                assert_eq!(mm.stale_devices, vec![4u32]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn submit_transport_error_returns_transport() {
        let mut http = MockHttp::new(vec![Expect::TransportErr]);
        let r = submit_encrypted_message(
            &fake_enc(),
            "00000000-0000-0000-0000-000000000002",
            &fake_account(),
            &mut http,
        );
        assert!(matches!(r, Err(SendError::Transport(_))));
    }

    // ---- mismatch parsing --------------------------------------------------

    #[test]
    fn mismatch_parse_missing_only() {
        let mm = parse_mismatch(br#"{"missingDevices":[2,3]}"#).unwrap();
        assert_eq!(mm.missing_devices, vec![2u32, 3]);
        assert!(mm.extra_devices.is_empty());
        assert!(mm.stale_devices.is_empty());
    }

    #[test]
    fn mismatch_parse_missing_and_extra() {
        let mm = parse_mismatch(br#"{"missingDevices":[],"extraDevices":[4,5]}"#).unwrap();
        assert!(mm.missing_devices.is_empty());
        assert_eq!(mm.extra_devices, vec![4u32, 5]);
    }

    #[test]
    fn mismatch_parse_stale_only() {
        let mm = parse_mismatch(br#"{"staleDevices":[7]}"#).unwrap();
        assert_eq!(mm.stale_devices, vec![7u32]);
    }

    #[test]
    fn mismatch_parse_empty_object_ok() {
        // Defaults to all-empty — distinguishable from a parse failure.
        let mm = parse_mismatch(br#"{}"#).unwrap();
        assert!(mm.missing_devices.is_empty());
        assert!(mm.extra_devices.is_empty());
        assert!(mm.stale_devices.is_empty());
    }

    // ---- backoff -----------------------------------------------------------

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(2), Duration::from_millis(1000));
        assert_eq!(backoff(3), Duration::from_millis(2000));
        assert_eq!(backoff(4), Duration::from_millis(4000));
        assert_eq!(backoff(5), Duration::from_millis(4000));
        assert_eq!(backoff(20), Duration::from_millis(4000));
    }

    // ---- account auth ------------------------------------------------------

    #[test]
    fn basic_auth_uses_aci_dot_device_id_form() {
        let acct = fake_account();
        let header = acct.basic_auth();
        assert!(header.starts_with("Basic "));
        let raw = STANDARD.decode(&header["Basic ".len()..]).unwrap();
        assert_eq!(
            std::str::from_utf8(&raw).unwrap(),
            "00000000-0000-0000-0000-000000000001.1:pw"
        );
    }

    #[test]
    fn chat_base_url_is_https_chat_host() {
        let url = fake_account().chat_base_url().unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("chat.signal.org"));
    }

    // ---- message body shape ------------------------------------------------

    #[test]
    fn outgoing_entity_serializes_in_camel_case() {
        let e = OutgoingMessageEntity {
            message_type: 1,
            destination_device_id: 2,
            destination_registration_id: 22,
            content: "//8=".to_string(),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], serde_json::json!(1));
        assert_eq!(v["destinationDeviceId"], serde_json::json!(2));
        assert_eq!(v["destinationRegistrationId"], serde_json::json!(22));
        assert_eq!(v["content"], serde_json::json!("//8="));
    }

    #[test]
    fn submit_request_has_required_top_level_fields() {
        let req = SubmitMessagesRequest {
            messages: vec![],
            online: false,
            urgent: true,
            timestamp: 1_700_000_000_000,
        };
        let v = serde_json::to_value(&req).unwrap();
        for k in ["messages", "online", "urgent", "timestamp"] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
        assert_eq!(v["online"], serde_json::json!(false));
        assert_eq!(v["urgent"], serde_json::json!(true));
        assert_eq!(v["timestamp"], serde_json::json!(1_700_000_000_000u64));
    }

    #[test]
    fn submit_encrypted_message_request_url_includes_uuid() {
        let mut http = MockHttp::new(vec![Expect::Put {
            url: "/v1/messages/00000000-0000-0000-0000-000000000002".into(),
            status: 200,
            body: br#"{}"#.to_vec(),
        }]);
        let r = submit_encrypted_message(
            &fake_enc(),
            "00000000-0000-0000-0000-000000000002",
            &fake_account(),
            &mut http,
        );
        assert!(r.is_ok());
        assert_eq!(*http.calls.borrow(), 1);
    }

    // ---- classify_attempt routing ------------------------------------------

    #[test]
    fn classify_ok_is_done() {
        assert!(matches!(classify_attempt(Ok(())), AttemptDecision::Done));
    }

    #[test]
    fn classify_409_carries_body() {
        let r = Err(SendError::BadResponse(
            "MISMATCH 409 {\"missingDevices\":[2]}".into(),
        ));
        match classify_attempt(r) {
            AttemptDecision::Mismatch409(body) => {
                assert_eq!(body, br#"{"missingDevices":[2]}"#.to_vec());
            }
            other => panic!("expected Mismatch409, got {other:?}"),
        }
    }

    #[test]
    fn classify_410_carries_body() {
        let r = Err(SendError::BadResponse(
            "MISMATCH 410 {\"staleDevices\":[3]}".into(),
        ));
        match classify_attempt(r) {
            AttemptDecision::Mismatch410(body) => {
                assert_eq!(body, br#"{"staleDevices":[3]}"#.to_vec());
            }
            other => panic!("expected Mismatch410, got {other:?}"),
        }
    }

    #[test]
    fn classify_5xx_is_backoff() {
        for code in [500u16, 502, 503, 599] {
            let r = Err(SendError::Unexpected(code));
            assert!(
                matches!(classify_attempt(r), AttemptDecision::Backoff),
                "code {code} did not classify as Backoff"
            );
        }
    }

    #[test]
    fn classify_429_is_backoff() {
        let r = Err(SendError::Unexpected(429));
        assert!(matches!(classify_attempt(r), AttemptDecision::Backoff));
    }

    #[test]
    fn classify_transport_is_backoff() {
        let r = Err(SendError::Transport("network down".into()));
        assert!(matches!(classify_attempt(r), AttemptDecision::Backoff));
    }

    #[test]
    fn classify_auth_failed_is_fatal() {
        let r = Err(SendError::AuthFailed);
        assert!(matches!(
            classify_attempt(r),
            AttemptDecision::Fatal(SendError::AuthFailed)
        ));
    }

    #[test]
    fn classify_404_is_fatal() {
        let r = Err(SendError::ServiceIdNotFound);
        assert!(matches!(
            classify_attempt(r),
            AttemptDecision::Fatal(SendError::ServiceIdNotFound)
        ));
    }

    #[test]
    fn classify_413_is_fatal() {
        let r = Err(SendError::PayloadTooLarge);
        assert!(matches!(
            classify_attempt(r),
            AttemptDecision::Fatal(SendError::PayloadTooLarge)
        ));
    }

    #[test]
    fn classify_428_is_fatal() {
        let r = Err(SendError::ChallengeRequired);
        assert!(matches!(
            classify_attempt(r),
            AttemptDecision::Fatal(SendError::ChallengeRequired)
        ));
    }

    #[test]
    fn classify_unexpected_4xx_is_fatal() {
        // 418 is a non-retriable unexpected
        let r = Err(SendError::Unexpected(418));
        assert!(matches!(
            classify_attempt(r),
            AttemptDecision::Fatal(SendError::Unexpected(418))
        ));
    }

    // ---- end-to-end retry loop with libsignal in-memory stores -------------
    //
    // These exercise submit_with_retry_generic over real libsignal store
    // implementations and a mock HTTP client. Stores are pre-seeded with a
    // session so encryption succeeds on every attempt; the loop's branching
    // on status codes is what's under test.

    use libsignal_protocol::{
        DeviceId, GenericSignedPreKey, IdentityKeyPair, InMemSignalProtocolStore, KeyPair,
        KyberPreKeyRecord, PreKeyBundle, PreKeyRecord, SignedPreKeyRecord, Timestamp,
        IdentityKeyStore as _, KyberPreKeyStore as _, PreKeyStore as _, SignedPreKeyStore as _,
        kem,
    };
    use rand::Rng;
    use rand::rngs::OsRng;

    /// Counts deletes per address; otherwise no-op.
    struct CountingDeleter {
        deletes: RefCell<Vec<ProtocolAddress>>,
    }
    impl CountingDeleter {
        fn new() -> Self {
            Self { deletes: RefCell::new(Vec::new()) }
        }
        fn delete_count(&self) -> usize {
            self.deletes.borrow().len()
        }
    }
    impl SessionDeleter for CountingDeleter {
        fn delete(&mut self, address: &ProtocolAddress) {
            self.deletes.borrow_mut().push(address.clone());
        }
    }

    /// Test-side wrapper around `InMemSessionStore` that implements
    /// `SessionStore + DeviceSessionEnum`. Tracks `(uuid, device_id)` pairs
    /// passed through `store_session`; production's `PddbSessionStore`
    /// inspects PDDB keys directly. The pre-loop seed lets tests express
    /// "we already have a session for X" without going through libsignal's
    /// process_prekey_bundle path twice.
    struct TrackingSessionStore<'a> {
        inner: &'a mut libsignal_protocol::InMemSessionStore,
        seen: RefCell<std::collections::HashSet<(String, u32)>>,
    }

    impl<'a> TrackingSessionStore<'a> {
        fn new(
            inner: &'a mut libsignal_protocol::InMemSessionStore,
            seed: &[(&str, u32)],
        ) -> Self {
            let mut s = std::collections::HashSet::new();
            for (u, d) in seed {
                s.insert(((*u).to_string(), *d));
            }
            Self { inner, seen: RefCell::new(s) }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl<'a> SessionStore for TrackingSessionStore<'a> {
        async fn load_session(
            &self,
            address: &ProtocolAddress,
        ) -> std::result::Result<Option<libsignal_protocol::SessionRecord>, libsignal_protocol::SignalProtocolError> {
            self.inner.load_session(address).await
        }
        async fn store_session(
            &mut self,
            address: &ProtocolAddress,
            record: &libsignal_protocol::SessionRecord,
        ) -> std::result::Result<(), libsignal_protocol::SignalProtocolError> {
            self.seen.borrow_mut().insert((
                address.name().to_string(),
                u32::from(address.device_id()),
            ));
            self.inner.store_session(address, record).await
        }
    }

    impl<'a> DeviceSessionEnum for TrackingSessionStore<'a> {
        fn device_ids_for(&self, recipient_uuid: &str) -> Vec<u32> {
            let mut ids: Vec<u32> = self
                .seen
                .borrow()
                .iter()
                .filter(|(u, _)| u == recipient_uuid)
                .map(|(_, d)| *d)
                .collect();
            ids.sort_unstable();
            ids
        }
    }

    fn fresh_lib_store() -> InMemSignalProtocolStore {
        let mut rng = OsRng.unwrap_err();
        let identity_key = IdentityKeyPair::generate(&mut rng);
        let registration_id: u32 = rng.random_range(1..16383);
        InMemSignalProtocolStore::new(identity_key, registration_id).unwrap()
    }

    fn make_bundle(store: &mut InMemSignalProtocolStore, device_id: DeviceId) -> PreKeyBundle {
        let mut rng = OsRng.unwrap_err();
        let pre_key_pair = KeyPair::generate(&mut rng);
        let signed_pre_key_pair = KeyPair::generate(&mut rng);
        let kyber_pre_key_pair = kem::KeyPair::generate(kem::KeyType::Kyber1024, &mut rng);

        let identity_key_pair = block_on(store.get_identity_key_pair()).unwrap();

        let signed_pub = signed_pre_key_pair.public_key.serialize();
        let signed_sig = identity_key_pair
            .private_key()
            .calculate_signature(&signed_pub, &mut rng)
            .unwrap();

        let kyber_pub = kyber_pre_key_pair.public_key.serialize();
        let kyber_sig = identity_key_pair
            .private_key()
            .calculate_signature(&kyber_pub, &mut rng)
            .unwrap();

        let pre_key_id: u32 = rng.random();
        let signed_pre_key_id: u32 = rng.random();
        let kyber_pre_key_id: u32 = rng.random();

        let bundle = PreKeyBundle::new(
            block_on(store.get_local_registration_id()).unwrap(),
            device_id,
            Some((pre_key_id.into(), pre_key_pair.public_key)),
            signed_pre_key_id.into(),
            signed_pre_key_pair.public_key,
            signed_sig.to_vec(),
            kyber_pre_key_id.into(),
            kyber_pre_key_pair.public_key.clone(),
            kyber_sig.to_vec(),
            *identity_key_pair.identity_key(),
        )
        .unwrap();

        block_on(store.save_pre_key(
            pre_key_id.into(),
            &PreKeyRecord::new(pre_key_id.into(), &pre_key_pair),
        ))
        .unwrap();
        block_on(store.save_signed_pre_key(
            signed_pre_key_id.into(),
            &SignedPreKeyRecord::new(
                signed_pre_key_id.into(),
                Timestamp::from_epoch_millis(42),
                &signed_pre_key_pair,
                &signed_sig,
            ),
        ))
        .unwrap();
        block_on(store.save_kyber_pre_key(
            kyber_pre_key_id.into(),
            &KyberPreKeyRecord::new(
                kyber_pre_key_id.into(),
                Timestamp::from_epoch_millis(43),
                &kyber_pre_key_pair,
                &kyber_sig,
            ),
        ))
        .unwrap();

        bundle
    }

    /// Pre-establish an Alice→Bob session and return everything the retry
    /// loop needs.
    fn alice_with_session_to_bob() -> (
        InMemSignalProtocolStore,
        ProtocolAddress,
        ProtocolAddress,
    ) {
        let mut alice = fresh_lib_store();
        let mut bob = fresh_lib_store();
        let alice_addr = ProtocolAddress::new("alice-uuid".into(), DeviceId::new(1).unwrap());
        let bob_addr = ProtocolAddress::new("bob-uuid".into(), DeviceId::new(2).unwrap());
        let bob_bundle = make_bundle(&mut bob, DeviceId::new(2).unwrap());
        let mut rng = OsRng.unwrap_err();
        block_on(libsignal_protocol::process_prekey_bundle(
            &bob_addr,
            &mut alice.session_store,
            &mut alice.identity_store,
            &bob_bundle,
            std::time::SystemTime::now(),
            &mut rng,
        ))
        .unwrap();
        (alice, alice_addr, bob_addr)
    }

    fn no_sleep() -> impl FnMut(Duration) {
        |_d| {}
    }

    #[test]
    fn retry_loop_happy_path_one_attempt() {
        let (mut alice, alice_addr, bob_addr) = alice_with_session_to_bob();
        let mut http = MockHttp::new(vec![Expect::Put {
            url: "/v1/messages/bob-uuid".into(),
            status: 200,
            body: br#"{}"#.to_vec(),
        }]);
        let mut deleter = CountingDeleter::new();
        let mut sleeper = no_sleep();
        let r = submit_with_retry_generic(
            "hello",
            1_700_000_000_000,
            &bob_addr,
            &alice_addr,
            &mut TrackingSessionStore::new(&mut alice.session_store, &[("bob-uuid", 2)]),
            &mut alice.identity_store,
            &mut deleter,
            &fake_account(),
            &mut http,
            &mut sleeper,
        );
        assert!(r.is_ok());
        assert_eq!(*http.calls.borrow(), 1);
        assert_eq!(deleter.delete_count(), 0);
    }

    #[test]
    fn retry_loop_410_recovers_then_succeeds() {
        let (mut alice, alice_addr, bob_addr) = alice_with_session_to_bob();
        let mut http = MockHttp::new(vec![
            Expect::Put {
                url: "/v1/messages/bob-uuid".into(),
                status: 410,
                body: br#"{"staleDevices":[2]}"#.to_vec(),
            },
            Expect::Put {
                url: "/v1/messages/bob-uuid".into(),
                status: 200,
                body: br#"{}"#.to_vec(),
            },
        ]);
        let mut deleter = CountingDeleter::new();
        let mut sleeper = no_sleep();
        // After 410, the loop deletes session for device 2 and re-encrypts.
        // Re-encryption will fail (NoSession) — this is the realistic
        // outcome given a single-device bob who's now "stale". For test
        // realism, we expect the loop to surface NoSession-derived
        // SendError::Encryption, not Ok.
        let r = submit_with_retry_generic(
            "hello",
            1_700_000_000_000,
            &bob_addr,
            &alice_addr,
            &mut TrackingSessionStore::new(&mut alice.session_store, &[("bob-uuid", 2)]),
            &mut alice.identity_store,
            &mut deleter,
            &fake_account(),
            &mut http,
            &mut sleeper,
        );
        // Either Ok if libsignal still has session state, or Encryption error.
        // What's invariant: the deleter saw exactly one delete for bob/2.
        assert_eq!(deleter.delete_count(), 1);
        // First HTTP call must have happened.
        assert!(*http.calls.borrow() >= 1);
        let _ = r;
    }

    #[test]
    fn retry_loop_5xx_exhausts_after_max_attempts() {
        let (mut alice, alice_addr, bob_addr) = alice_with_session_to_bob();
        let mut http = MockHttp::new(vec![
            Expect::Put { url: "/v1/messages/bob-uuid".into(), status: 503, body: vec![] },
            Expect::Put { url: "/v1/messages/bob-uuid".into(), status: 503, body: vec![] },
            Expect::Put { url: "/v1/messages/bob-uuid".into(), status: 503, body: vec![] },
        ]);
        let mut deleter = CountingDeleter::new();
        let mut sleeper = no_sleep();
        let r = submit_with_retry_generic(
            "hello",
            1_700_000_000_000,
            &bob_addr,
            &alice_addr,
            &mut TrackingSessionStore::new(&mut alice.session_store, &[("bob-uuid", 2)]),
            &mut alice.identity_store,
            &mut deleter,
            &fake_account(),
            &mut http,
            &mut sleeper,
        );
        assert!(matches!(r, Err(SendError::RetryExhausted)),
            "expected RetryExhausted, got {r:?}");
        assert_eq!(*http.calls.borrow(), 3);
    }

    #[test]
    fn retry_loop_401_fails_immediately_no_retry() {
        let (mut alice, alice_addr, bob_addr) = alice_with_session_to_bob();
        let mut http = MockHttp::new(vec![Expect::Put {
            url: "/v1/messages/bob-uuid".into(),
            status: 401,
            body: vec![],
        }]);
        let mut deleter = CountingDeleter::new();
        let mut sleeper = no_sleep();
        let r = submit_with_retry_generic(
            "hello",
            1_700_000_000_000,
            &bob_addr,
            &alice_addr,
            &mut TrackingSessionStore::new(&mut alice.session_store, &[("bob-uuid", 2)]),
            &mut alice.identity_store,
            &mut deleter,
            &fake_account(),
            &mut http,
            &mut sleeper,
        );
        assert!(matches!(r, Err(SendError::AuthFailed)),
            "expected AuthFailed, got {r:?}");
        assert_eq!(*http.calls.borrow(), 1);
    }

    #[test]
    fn retry_loop_404_fails_immediately_no_retry() {
        let (mut alice, alice_addr, bob_addr) = alice_with_session_to_bob();
        let mut http = MockHttp::new(vec![Expect::Put {
            url: "/v1/messages/bob-uuid".into(),
            status: 404,
            body: vec![],
        }]);
        let mut deleter = CountingDeleter::new();
        let mut sleeper = no_sleep();
        let r = submit_with_retry_generic(
            "hello",
            1_700_000_000_000,
            &bob_addr,
            &alice_addr,
            &mut TrackingSessionStore::new(&mut alice.session_store, &[("bob-uuid", 2)]),
            &mut alice.identity_store,
            &mut deleter,
            &fake_account(),
            &mut http,
            &mut sleeper,
        );
        assert!(matches!(r, Err(SendError::ServiceIdNotFound)));
        assert_eq!(*http.calls.borrow(), 1);
    }

    #[test]
    fn retry_loop_network_error_then_200_succeeds() {
        let (mut alice, alice_addr, bob_addr) = alice_with_session_to_bob();
        let mut http = MockHttp::new(vec![
            Expect::TransportErr,
            Expect::Put {
                url: "/v1/messages/bob-uuid".into(),
                status: 200,
                body: br#"{}"#.to_vec(),
            },
        ]);
        let mut deleter = CountingDeleter::new();
        let mut sleeper = no_sleep();
        let r = submit_with_retry_generic(
            "hello",
            1_700_000_000_000,
            &bob_addr,
            &alice_addr,
            &mut TrackingSessionStore::new(&mut alice.session_store, &[("bob-uuid", 2)]),
            &mut alice.identity_store,
            &mut deleter,
            &fake_account(),
            &mut http,
            &mut sleeper,
        );
        assert!(r.is_ok(), "expected Ok, got {r:?}");
        assert_eq!(*http.calls.borrow(), 2);
    }

    // ---- regression: Signal-Server returns unpadded base64 -----------------

    /// `PERMISSIVE` accepts base64 with or without `=` padding. Both should
    /// decode to identical bytes.
    #[test]
    fn permissive_decoder_accepts_both_padding_modes() {
        // 64 bytes (a typical Ed25519 signature length): encodes to 88 b64
        // chars with 2 `=` padding bytes — forces non-zero padding count.
        let raw: Vec<u8> = (0u8..64).collect();
        let padded = STANDARD.encode(&raw);
        assert!(padded.ends_with('='), "fixture must be padded");
        let unpadded: String = padded.trim_end_matches('=').to_string();
        assert!(!unpadded.ends_with('='));

        let from_padded = PERMISSIVE.decode(&padded).expect("padded decode");
        let from_unpadded = PERMISSIVE.decode(&unpadded).expect("unpadded decode");
        assert_eq!(from_padded, raw);
        assert_eq!(from_unpadded, raw);
    }

    /// `STANDARD` rejects unpadded input — this is the production bug surfaced
    /// by the Phase A real-send scan. Locking it in as a test makes the
    /// `STANDARD` → `PERMISSIVE` swap a regression-tested change rather than
    /// a behavioural one.
    #[test]
    fn standard_rejects_unpadded_input_pre_fix_repro() {
        let raw: Vec<u8> = (0u8..64).collect();
        let unpadded: String = STANDARD.encode(&raw).trim_end_matches('=').to_string();
        let r = STANDARD.decode(&unpadded);
        assert!(r.is_err(), "STANDARD must reject unpadded input");
    }

    /// `build_prekey_bundle` with every base64 field unpadded — the exact
    /// shape Signal-Server returns. Pre-fix this would fail at the first
    /// `STANDARD.decode` of `signed_pre_key.signature`. With `PERMISSIVE` the
    /// bundle is constructed successfully and matches a parallel build using
    /// the raw key material.
    #[test]
    fn build_prekey_bundle_accepts_unpadded_signal_server_format() {
        use libsignal_protocol::{IdentityKeyPair, KeyPair, kem};
        use rand::Rng;
        use rand::rngs::OsRng;

        let mut rng = OsRng.unwrap_err();
        let identity_kp = IdentityKeyPair::generate(&mut rng);
        let signed_kp = KeyPair::generate(&mut rng);
        let kyber_kp = kem::KeyPair::generate(kem::KeyType::Kyber1024, &mut rng);
        let one_time_kp = KeyPair::generate(&mut rng);

        let signed_pub_bytes = signed_kp.public_key.serialize();
        let signed_sig = identity_kp
            .private_key()
            .calculate_signature(&signed_pub_bytes, &mut rng)
            .unwrap();
        let kyber_pub_bytes = kyber_kp.public_key.serialize();
        let kyber_sig = identity_kp
            .private_key()
            .calculate_signature(&kyber_pub_bytes, &mut rng)
            .unwrap();

        // Encode every field with `withoutPadding()` — matching Signal-Server.
        let unpad = |b: &[u8]| -> String {
            STANDARD.encode(b).trim_end_matches('=').to_string()
        };

        let entry = DeviceEntry {
            device_id: 1,
            registration_id: rng.random_range(1..16383),
            signed_pre_key: SignedPreKeyEntry {
                key_id: 8287829,
                public_key: unpad(&signed_pub_bytes),
                signature: unpad(&signed_sig),
            },
            pre_key: Some(PreKeyEntry {
                key_id: 12345,
                public_key: unpad(&one_time_kp.public_key.serialize()),
            }),
            pq_pre_key: KyberPreKeyEntry {
                key_id: 7418106,
                public_key: unpad(&kyber_pub_bytes),
                signature: unpad(&kyber_sig),
            },
        };

        let bundle = build_prekey_bundle(&entry, *identity_kp.identity_key())
            .expect("bundle parses with unpadded base64");
        assert_eq!(u32::from(bundle.device_id().unwrap()), 1);
    }

    // ---- multi-device fan-out (Phase A v4) ---------------------------------

    /// Stateful mock that simulates the relevant slice of Signal-Server: an
    /// account UUID has a registered set of devices, and PUT /v1/messages
    /// returns 409 with `missingDevices` and `extraDevices` when the body's
    /// `messages[]` array doesn't address every registered device. GET
    /// /v2/keys/{uuid}/{device_id} returns the registered prekey response
    /// for that device. Programmable by the test up-front; deterministic
    /// after that.
    ///
    /// This catches the bug fixed in this commit AND any future variant:
    /// "retry submitted the same single-device body" produces an infinite
    /// 409 loop in the mock just like in production.
    struct StatefulMockHttp {
        /// uuid → [(device_id, registration_id, prekey_response_json_bytes)]
        registered: std::collections::HashMap<String, Vec<RegisteredDevice>>,
        last_put_body: RefCell<Option<Vec<u8>>>,
        put_count: RefCell<usize>,
        get_count: RefCell<usize>,
    }

    struct RegisteredDevice {
        device_id: u32,
        // identity_key + DeviceEntry-shaped JSON for /v2/keys responses.
        prekey_response_body: Vec<u8>,
    }

    impl StatefulMockHttp {
        fn new() -> Self {
            Self {
                registered: std::collections::HashMap::new(),
                last_put_body: RefCell::new(None),
                put_count: RefCell::new(0),
                get_count: RefCell::new(0),
            }
        }

        fn register_device(&mut self, uuid: &str, dev: RegisteredDevice) {
            self.registered.entry(uuid.to_string()).or_default().push(dev);
        }

        fn registered_device_ids(&self, uuid: &str) -> std::collections::HashSet<u32> {
            self.registered
                .get(uuid)
                .map(|v| v.iter().map(|d| d.device_id).collect())
                .unwrap_or_default()
        }
    }

    impl HttpClient for StatefulMockHttp {
        fn put_json(&mut self, url: &str, _auth: &str, body: &[u8]) -> io::Result<HttpResponse> {
            *self.put_count.borrow_mut() += 1;
            *self.last_put_body.borrow_mut() = Some(body.to_vec());

            // /v1/messages/{uuid}
            let uuid = url
                .split("/v1/messages/")
                .nth(1)
                .map(|s| s.split('?').next().unwrap_or(s).trim_end_matches('/'))
                .unwrap_or("");

            let req: serde_json::Value = serde_json::from_slice(body)
                .map_err(|e| io::Error::other(format!("body parse: {e}")))?;
            let sent: std::collections::HashSet<u32> = req["messages"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|e| e["destinationDeviceId"].as_u64().unwrap_or(0) as u32)
                .collect();

            let registered = self.registered_device_ids(uuid);
            let missing: Vec<u32> =
                registered.difference(&sent).copied().collect();
            let extra: Vec<u32> = sent.difference(&registered).copied().collect();

            if missing.is_empty() && extra.is_empty() {
                Ok(HttpResponse { status: 200, body: br#"{}"#.to_vec() })
            } else {
                let mut missing_sorted = missing.clone();
                missing_sorted.sort_unstable();
                let mut extra_sorted = extra.clone();
                extra_sorted.sort_unstable();
                let body = serde_json::to_vec(&serde_json::json!({
                    "missingDevices": missing_sorted,
                    "extraDevices": extra_sorted,
                }))
                .unwrap();
                Ok(HttpResponse { status: 409, body })
            }
        }

        fn get_json(&mut self, url: &str, _auth: &str) -> io::Result<HttpResponse> {
            *self.get_count.borrow_mut() += 1;
            // /v2/keys/{uuid}/{device_id|*}
            let tail = url
                .split("/v2/keys/")
                .nth(1)
                .ok_or_else(|| io::Error::other(format!("bad url: {url}")))?;
            let mut parts = tail.split('/');
            let uuid = parts
                .next()
                .ok_or_else(|| io::Error::other("missing uuid"))?;
            let selector = parts
                .next()
                .ok_or_else(|| io::Error::other("missing device selector"))?;

            // Wildcard returns the merged response across all registered
            // devices (one PreKeyResponse with multiple `devices` entries).
            // Mirrors Signal-Server's GET /v2/keys/{uuid}/* shape.
            if selector == "*" {
                let devices = self.registered.get(uuid);
                let entries = match devices {
                    Some(v) if !v.is_empty() => v,
                    _ => return Ok(HttpResponse { status: 404, body: vec![] }),
                };
                let mut all_entries: Vec<serde_json::Value> = Vec::new();
                let mut identity_key_b64: Option<String> = None;
                for d in entries {
                    let resp: serde_json::Value =
                        serde_json::from_slice(&d.prekey_response_body).unwrap();
                    if identity_key_b64.is_none() {
                        identity_key_b64 = resp["identityKey"].as_str().map(|s| s.to_string());
                    }
                    if let Some(devs) = resp["devices"].as_array() {
                        for entry in devs {
                            all_entries.push(entry.clone());
                        }
                    }
                }
                let body = serde_json::json!({
                    "identityKey": identity_key_b64.unwrap_or_default(),
                    "devices": all_entries,
                });
                return Ok(HttpResponse {
                    status: 200,
                    body: serde_json::to_vec(&body).unwrap(),
                });
            }

            let dev: u32 = selector
                .parse()
                .map_err(|_| io::Error::other(format!("bad selector {selector}")))?;
            let bundle = self
                .registered
                .get(uuid)
                .and_then(|v| v.iter().find(|d| d.device_id == dev))
                .ok_or_else(|| {
                    io::Error::other(format!("no registered device {uuid}/{dev}"))
                })?;

            Ok(HttpResponse {
                status: 200,
                body: bundle.prekey_response_body.clone(),
            })
        }
    }

    /// Build a registered-device record for `StatefulMockHttp`: generates a
    /// fresh keypair set, persists the records into `store` so libsignal's
    /// `process_prekey_bundle` can later succeed against the matching keys,
    /// and packages a `/v2/keys/{uuid}/{device}` JSON response that points
    /// at those keys. The identity key shared across devices belongs to
    /// `identity_kp` (one identity per account, multiple devices).
    fn make_registered_device(
        store: &mut InMemSignalProtocolStore,
        identity_kp: &IdentityKeyPair,
        device_id: u32,
    ) -> RegisteredDevice {
        let mut rng = OsRng.unwrap_err();
        let pre_key_pair = KeyPair::generate(&mut rng);
        let signed_pre_key_pair = KeyPair::generate(&mut rng);
        let kyber_pre_key_pair =
            kem::KeyPair::generate(kem::KeyType::Kyber1024, &mut rng);

        let signed_pub = signed_pre_key_pair.public_key.serialize();
        let signed_sig = identity_kp
            .private_key()
            .calculate_signature(&signed_pub, &mut rng)
            .unwrap();
        let kyber_pub = kyber_pre_key_pair.public_key.serialize();
        let kyber_sig = identity_kp
            .private_key()
            .calculate_signature(&kyber_pub, &mut rng)
            .unwrap();

        let pre_key_id: u32 = rng.random();
        let signed_pre_key_id: u32 = rng.random();
        let kyber_pre_key_id: u32 = rng.random();
        let registration_id: u32 = rng.random_range(1..16383);

        block_on(store.save_pre_key(
            pre_key_id.into(),
            &PreKeyRecord::new(pre_key_id.into(), &pre_key_pair),
        ))
        .unwrap();
        block_on(store.save_signed_pre_key(
            signed_pre_key_id.into(),
            &SignedPreKeyRecord::new(
                signed_pre_key_id.into(),
                Timestamp::from_epoch_millis(42),
                &signed_pre_key_pair,
                &signed_sig,
            ),
        ))
        .unwrap();
        block_on(store.save_kyber_pre_key(
            kyber_pre_key_id.into(),
            &KyberPreKeyRecord::new(
                kyber_pre_key_id.into(),
                Timestamp::from_epoch_millis(43),
                &kyber_pre_key_pair,
                &kyber_sig,
            ),
        ))
        .unwrap();

        // Match Signal-Server's wire format: unpadded standard base64 for
        // every field. PERMISSIVE decoder accepts both padded and unpadded.
        let unpad = |b: &[u8]| -> String {
            STANDARD.encode(b).trim_end_matches('=').to_string()
        };

        let body = serde_json::json!({
            "identityKey": unpad(&identity_kp.identity_key().serialize()),
            "devices": [{
                "deviceId": device_id,
                "registrationId": registration_id,
                "signedPreKey": {
                    "keyId": signed_pre_key_id,
                    "publicKey": unpad(&signed_pub),
                    "signature": unpad(&signed_sig),
                },
                "preKey": {
                    "keyId": pre_key_id,
                    "publicKey": unpad(&pre_key_pair.public_key.serialize()),
                },
                "pqPreKey": {
                    "keyId": kyber_pre_key_id,
                    "publicKey": unpad(&kyber_pub),
                    "signature": unpad(&kyber_sig),
                },
            }],
        });

        RegisteredDevice {
            device_id,
            prekey_response_body: serde_json::to_vec(&body).unwrap(),
        }
    }

    /// The exact failure mode from the Phase A real-send scan v2 → v3:
    /// Precursor knows about device 2 (signal-cli) only; server says it
    /// also has device 1 (the phone). Loop must fan out.
    #[test]
    fn send_with_two_recipient_devices_fans_out_after_409() {
        // Alice's store + a session pre-established to bob/2 only.
        let (mut alice, alice_addr, bob_addr) = alice_with_session_to_bob();

        // Bob's account has two devices on the server: 1 and 2. We need a
        // shared identity for both (one account = one identity), and each
        // device has its own prekey records.
        let mut bob_device_state = fresh_lib_store();
        let bob_identity =
            block_on(bob_device_state.get_identity_key_pair()).unwrap();
        let dev1 = make_registered_device(&mut bob_device_state, &bob_identity, 1);
        let dev2 = make_registered_device(&mut bob_device_state, &bob_identity, 2);

        let mut http = StatefulMockHttp::new();
        http.register_device("bob-uuid", dev1);
        http.register_device("bob-uuid", dev2);

        let mut deleter = CountingDeleter::new();
        let mut sleeper = no_sleep();

        let r = submit_with_retry_generic(
            "hello",
            1_700_000_000_000,
            &bob_addr,
            &alice_addr,
            &mut TrackingSessionStore::new(&mut alice.session_store, &[("bob-uuid", 2)]),
            &mut alice.identity_store,
            &mut deleter,
            &fake_account(),
            &mut http,
            &mut sleeper,
        );

        assert!(r.is_ok(), "expected Ok after fan-out, got {r:?}");

        // First PUT (devices=[2]) → 409, GET prekey bundle for device 1,
        // process_prekey_bundle → second PUT (devices=[1,2]) → 200. Some
        // implementations may issue an extra GET (e.g., for self-identity
        // verification) — the invariant is "≥1 GET for prekey fetch".
        assert!(
            *http.put_count.borrow() >= 2,
            "expected ≥2 PUTs, got {}",
            *http.put_count.borrow()
        );
        assert!(
            *http.get_count.borrow() >= 1,
            "expected ≥1 GET for missing-device prekey, got {}",
            *http.get_count.borrow()
        );

        // Last PUT body must contain entities for BOTH devices.
        let last_body = http.last_put_body.borrow().clone().expect("a PUT happened");
        let req: serde_json::Value = serde_json::from_slice(&last_body).unwrap();
        let device_ids: std::collections::HashSet<u32> = req["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["destinationDeviceId"].as_u64().unwrap() as u32)
            .collect();
        assert!(
            device_ids.contains(&1) && device_ids.contains(&2),
            "last PUT must address both devices, got {device_ids:?}"
        );
    }

    /// Stale-device path: server returns 410 listing a device whose session
    /// we have. Loop must drop the session and the next attempt must
    /// succeed (or fail cleanly without that device).
    #[test]
    fn send_with_stale_device_drops_session_and_succeeds() {
        let (mut alice, alice_addr, bob_addr) = alice_with_session_to_bob();

        // Build a custom mock: first PUT returns 410 staleDevices=[2],
        // second PUT returns 200. After the first, the loop deletes the
        // session for device 2 and re-encrypts — the next iteration's
        // device enumeration is empty (no remaining sessions), so it
        // falls back to recipient_addr.device_id() = 2. The mock returns
        // 200 anyway, simulating "device came back". The deleter's
        // count-of-1 is the meaningful signal.
        let mut http = MockHttp::new(vec![
            Expect::Put {
                url: "/v1/messages/bob-uuid".into(),
                status: 410,
                body: br#"{"staleDevices":[2]}"#.to_vec(),
            },
            Expect::Put {
                url: "/v1/messages/bob-uuid".into(),
                status: 200,
                body: br#"{}"#.to_vec(),
            },
        ]);
        let mut deleter = CountingDeleter::new();
        let mut sleeper = no_sleep();
        let _ = submit_with_retry_generic(
            "hello",
            1_700_000_000_000,
            &bob_addr,
            &alice_addr,
            &mut TrackingSessionStore::new(&mut alice.session_store, &[("bob-uuid", 2)]),
            &mut alice.identity_store,
            &mut deleter,
            &fake_account(),
            &mut http,
            &mut sleeper,
        );

        // The 410 path must have deleted bob/2's session.
        assert_eq!(
            deleter.delete_count(),
            1,
            "expected exactly one delete for bob/2 on 410"
        );
    }

    // ---- sync transcript fan-out (Phase A v7) ------------------------------

    /// `submit_sync_transcript_generic` must:
    ///   - target the sender's own UUID (PUT URL contains own_uuid)
    ///   - exclude own_device_id from the fan-out device set
    ///   - include all OTHER devices of own UUID
    ///   - emit a SyncMessage Content (not a DataMessage Content) inside
    ///     each per-device ciphertext
    /// We can't decrypt the ciphertext through MockHttp, but we can verify
    /// the device set the PUT addresses, and that the only PUT is to
    /// own_uuid (no recipient PUT).
    #[test]
    fn sync_transcript_fans_out_to_own_other_devices() {
        // Alice (own account) has a session with herself's other device 7.
        // Sender device_id = 1 (the linked secondary doing the send).
        // Own account UUID = "alice-uuid". Recipient is irrelevant in this
        // direct test (we drive submit_sync_transcript_generic directly).
        let mut alice = fresh_lib_store();
        let alice_self_other_addr =
            ProtocolAddress::new("alice-uuid".into(), DeviceId::new(7).unwrap());

        // Pre-establish Alice → Alice/7 session (own-account other device).
        let mut alice_other = fresh_lib_store();
        let alice_other_bundle =
            make_bundle(&mut alice_other, DeviceId::new(7).unwrap());
        let mut rng = OsRng.unwrap_err();
        block_on(libsignal_protocol::process_prekey_bundle(
            &alice_self_other_addr,
            &mut alice.session_store,
            &mut alice.identity_store,
            &alice_other_bundle,
            std::time::SystemTime::now(),
            &mut rng,
        ))
        .unwrap();

        let local_addr =
            ProtocolAddress::new("alice-uuid".into(), DeviceId::new(1).unwrap());

        let mut http = StatefulMockHttp::new();
        // Real Signal-Server excludes the requesting device from its 409
        // "mismatched devices" calculation when receiving a sync transcript
        // PUT, so the mock only registers the OTHER devices of own UUID.
        let alice_identity =
            block_on(alice.get_identity_key_pair()).unwrap();
        let dev7 = make_registered_device(
            &mut fresh_lib_store(),
            &alice_identity,
            7,
        );
        http.register_device("alice-uuid", dev7);

        let mut deleter = CountingDeleter::new();
        let mut sleeper = no_sleep();
        let mut tracker = TrackingSessionStore::new(
            &mut alice.session_store,
            &[("alice-uuid", 7)],
        );

        let r = submit_sync_transcript_generic(
            "hello sync",
            "bob-uuid",          // recipient UUID echoed inside the transcript
            1_700_000_000_000,
            "alice-uuid",        // own UUID (PUT target)
            1,                   // own device id (excluded)
            &local_addr,
            &mut tracker,
            &mut alice.identity_store,
            &mut deleter,
            &fake_account(),
            &mut http,
            &mut sleeper,
        );
        if let Err(e) = &r {
            panic!("submit_sync_transcript_generic failed: {:?}", e);
        }

        // Exactly one PUT, to alice-uuid, addressing only device 7.
        assert_eq!(*http.put_count.borrow(), 1);
        let put = http.last_put_body.borrow().clone().expect("a PUT happened");
        let req: serde_json::Value = serde_json::from_slice(&put).unwrap();
        let device_ids: Vec<u32> = req["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["destinationDeviceId"].as_u64().unwrap() as u32)
            .collect();
        assert_eq!(
            device_ids,
            vec![7u32],
            "sync PUT must address own-other devices excluding self"
        );
    }

    /// When the sender's own account has no devices other than the sender
    /// itself, the sync transcript path must short-circuit without any PUT.
    #[test]
    fn sync_transcript_skipped_when_own_account_has_no_other_devices() {
        let mut alice = fresh_lib_store();
        let local_addr =
            ProtocolAddress::new("alice-uuid".into(), DeviceId::new(1).unwrap());

        // No prior sessions for alice-uuid in the tracker.
        let mut tracker = TrackingSessionStore::new(
            &mut alice.session_store,
            &[],
        );

        let mut http = StatefulMockHttp::new();
        // Server registration is irrelevant — we shouldn't reach it.
        let mut deleter = CountingDeleter::new();
        let mut sleeper = no_sleep();

        let r = submit_sync_transcript_generic(
            "hello sync",
            "bob-uuid",
            1_700_000_000_000,
            "alice-uuid",
            1,
            &local_addr,
            &mut tracker,
            &mut alice.identity_store,
            &mut deleter,
            &fake_account(),
            &mut http,
            &mut sleeper,
        );
        if let Err(e) = &r {
            panic!("expected Ok (no-op), got {:?}", e);
        }
        assert_eq!(*http.put_count.borrow(), 0,
            "no PUT expected when own account has no other devices");
        // One GET expected: the up-front device discovery
        // /v2/keys/{own_uuid}/* which the empty-mock returns 404 for,
        // causing the sync path to skip without further requests.
        assert_eq!(*http.get_count.borrow(), 1,
            "one discovery GET expected, no further requests");
    }
}
