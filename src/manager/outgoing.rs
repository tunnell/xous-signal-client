//! Outgoing message encryption — Phase 2a.
//!
//! Builds the encrypted ciphertext + envelope metadata for a 1:1 message,
//! up to but not including the HTTP submission. Phase 2b takes the
//! [`EncryptedMessage`] this returns and submits it via PUT /v1/messages/{uuid}.
//!
//! Encryption flow:
//!   1. Build Content { DataMessage { body, timestamp } }
//!   2. Apply Signal application-layer padding (content + 0x80 + 0x00*N to
//!      next multiple of 160). Inverse of [`strip_signal_padding`] on receive.
//!   3. Look up SessionRecord for recipient → extract remote_registration_id.
//!   4. Call libsignal_protocol::message_encrypt → returns CiphertextMessage.
//!   5. Map enum variant to envelope type code:
//!        SignalMessage          → 1 (CIPHERTEXT)
//!        PreKeySignalMessage    → 3 (PREKEY_BUNDLE)
//!
//! Recipient resolution: V1 uses pddb-persisted "default.peer" (UUID +
//! device_id) populated by the receive path. Single-conversation
//! scaffolding; per-conversation routing is V2.

#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]

use std::io::{Read, Write};
use std::time::SystemTime;
use futures::executor::block_on;
use libsignal_protocol::{
    CiphertextMessage, DeviceId, IdentityKeyStore, ProtocolAddress,
    SessionStore,
    message_encrypt,
};
use prost::Message as ProstMessage;
use rand::TryRngCore as _;

use crate::manager::stores::{PddbIdentityStore, PddbSessionStore};

const ACCOUNT_DICT: &'static str = "sigchat.account";
const IDENTITY_DICT: &'static str = "sigchat.identity";
const SESSION_DICT: &'static str = "sigchat.session";

const DIALOGUE_DICT: &'static str = "sigchat.dialogue";
const DEFAULT_PEER_KEY: &'static str = "default.peer";

const ACI_SERVICE_ID_KEY: &'static str = "aci.service_id";
const DEVICE_ID_KEY: &'static str = "device_id";

/// Signal envelope.type codes used by the wire protocol. These are emitted
/// by the receive path's dispatch (main_ws.rs) and must match here.
pub(crate) const ENVELOPE_CIPHERTEXT: i32 = 1;
pub(crate) const ENVELOPE_PREKEY_BUNDLE: i32 = 3;

// ---- Inline prost definitions (mirror main_ws.rs receive types) ------------

// Per Signal's canonical SignalService.proto, DataMessage.timestamp is at
// proto field 7 (uint64), not 5. Field 5 is `expireTimer` (uint32). Prior
// versions emitted timestamp at tag 5, leaving the canonical timestamp
// field absent on the wire — recipients dropped the message via
// EnvelopeContentValidator's "Missing timestamp" check.
//
// Refs: REPORTS/XOUS-SIGNAL-CLIENT-PHASEA-AUDIT.md (B1).
#[derive(prost::Message)]
struct DataMessageProto {
    #[prost(string, optional, tag = "1")]
    body: Option<String>,
    #[prost(uint64, optional, tag = "7")]
    timestamp: Option<u64>,
}

// SyncMessage.Sent (signalservice.proto). Field tags per canonical proto:
//   destinationE164      = 1   (omitted; we use destinationServiceId)
//   timestamp            = 2
//   message              = 3   (DataMessage)
//   destinationServiceId = 7
#[derive(prost::Message)]
struct SentMessageProto {
    #[prost(uint64, optional, tag = "2")]
    timestamp: Option<u64>,
    #[prost(message, optional, tag = "3")]
    message: Option<DataMessageProto>,
    #[prost(string, optional, tag = "7")]
    destination_service_id: Option<String>,
}

// SyncMessage container. SyncMessage.sent = 1.
#[derive(prost::Message)]
struct SyncMessageProto {
    #[prost(message, optional, tag = "1")]
    sent: Option<SentMessageProto>,
}

// Content (signalservice.proto). Content.dataMessage = 1, Content.syncMessage = 2.
#[derive(prost::Message)]
struct ContentProto {
    #[prost(message, optional, tag = "1")]
    data_message: Option<DataMessageProto>,
    #[prost(message, optional, tag = "2")]
    sync_message: Option<SyncMessageProto>,
}

// ---- Public types -----------------------------------------------------------

#[derive(Debug)]
pub(crate) struct EncryptedMessage {
    pub ciphertext_bytes: Vec<u8>,
    pub ciphertext_type: i32,
    pub destination_device_id: u32,
    pub destination_registration_id: u32,
    pub timestamp_ms: u64,
}

#[derive(Debug)]
pub enum OutgoingError {
    Pddb(String),
    NoRecipient,
    NoLocalAccount(String),
    SessionLoad(String),
    NoSession,
    RegistrationId(String),
    Encrypt(String),
    UnsupportedCiphertextType(String),
    BadDeviceId(u32),
}

impl std::fmt::Display for OutgoingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pddb(s) => write!(f, "pddb: {s}"),
            Self::NoRecipient => write!(f, "no current recipient (no peer has messaged us yet)"),
            Self::NoLocalAccount(s) => write!(f, "local account: {s}"),
            Self::SessionLoad(s) => write!(f, "session load: {s}"),
            Self::NoSession => write!(f, "no session for recipient"),
            Self::RegistrationId(s) => write!(f, "registration_id: {s}"),
            Self::Encrypt(s) => write!(f, "encrypt: {s}"),
            Self::UnsupportedCiphertextType(s) => write!(f, "unsupported ciphertext type: {s}"),
            Self::BadDeviceId(d) => write!(f, "device_id {d} out of range (1..=127)"),
        }
    }
}

// ---- [Phase A v5 audit] uncommitted diagnostic dump -----------------------

#[allow(dead_code)]
fn dump_hex(label: &str, ts: u64, bytes: &[u8]) -> std::io::Result<()> {
    use std::fmt::Write as _;
    use std::io::Write as _;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(hex, "{:02x}", b);
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/xsc-wire-dump.txt")?;
    writeln!(f, "[{ts}] {label} (len={}): {}", bytes.len(), hex)?;
    Ok(())
}

// ---- Padding (inverse of strip_signal_padding in main_ws.rs) ---------------

pub(crate) fn signal_pad(content: &mut Vec<u8>) {
    content.push(0x80);
    while content.len() % 160 != 0 {
        content.push(0x00);
    }
}

// ---- Content builders ------------------------------------------------------

/// Build the padded `Content { dataMessage }` for an outbound text message.
/// Caller passes the result to `encrypt_padded_for_recipient`.
pub(crate) fn build_padded_data_message_content(
    plaintext_body: &str,
    timestamp_ms: u64,
) -> Vec<u8> {
    let dm = DataMessageProto {
        body: Some(plaintext_body.to_string()),
        timestamp: Some(timestamp_ms),
    };
    let content = ContentProto {
        data_message: Some(dm),
        sync_message: None,
    };
    let mut bytes = content.encode_to_vec();
    if std::env::var("XSCDEBUG_DUMP").is_ok() {
        let _ = dump_hex(
            "Content protobuf (DataMessage, pre-encrypt, pre-pad)",
            timestamp_ms,
            &bytes,
        );
    }
    signal_pad(&mut bytes);
    if std::env::var("XSCDEBUG_DUMP").is_ok() {
        let _ = dump_hex(
            "Padded plaintext (DataMessage, post-pad, pre-encrypt)",
            timestamp_ms,
            &bytes,
        );
    }
    bytes
}

/// Build the padded `Content { syncMessage { sent } }` for the sync transcript
/// that the sender's own account's other devices receive after an outbound
/// send. Wraps a copy of the original DataMessage; recipient-side service ID
/// and timestamp echo through.
pub(crate) fn build_padded_sync_transcript_content(
    recipient_uuid: &str,
    plaintext_body: &str,
    timestamp_ms: u64,
) -> Vec<u8> {
    let inner_dm = DataMessageProto {
        body: Some(plaintext_body.to_string()),
        timestamp: Some(timestamp_ms),
    };
    let sent = SentMessageProto {
        timestamp: Some(timestamp_ms),
        message: Some(inner_dm),
        destination_service_id: Some(recipient_uuid.to_string()),
    };
    let sync = SyncMessageProto { sent: Some(sent) };
    let content = ContentProto {
        data_message: None,
        sync_message: Some(sync),
    };
    let mut bytes = content.encode_to_vec();
    if std::env::var("XSCDEBUG_DUMP").is_ok() {
        let _ = dump_hex(
            "Content protobuf (SyncMessage::Sent, pre-encrypt, pre-pad)",
            timestamp_ms,
            &bytes,
        );
    }
    signal_pad(&mut bytes);
    if std::env::var("XSCDEBUG_DUMP").is_ok() {
        let _ = dump_hex(
            "Padded plaintext (SyncMessage::Sent, post-pad, pre-encrypt)",
            timestamp_ms,
            &bytes,
        );
    }
    bytes
}

/// Encrypt pre-padded Content bytes for a single recipient device. Used for
/// both the main DataMessage send path and the sync transcript path; the
/// content variant is encoded in the plaintext bytes themselves.
pub(crate) fn encrypt_padded_for_recipient<S, I>(
    padded_content: &[u8],
    timestamp_ms: u64,
    recipient_addr: &ProtocolAddress,
    local_addr: &ProtocolAddress,
    session_store: &mut S,
    identity_store: &mut I,
) -> Result<EncryptedMessage, OutgoingError>
where
    S: SessionStore,
    I: IdentityKeyStore,
{
    let session_record = block_on(session_store.load_session(recipient_addr))
        .map_err(|e| OutgoingError::SessionLoad(format!("{e:?}")))?
        .ok_or(OutgoingError::NoSession)?;
    let dest_reg_id = session_record
        .remote_registration_id()
        .map_err(|e| OutgoingError::RegistrationId(format!("{e:?}")))?;

    let mut rng = rand::rngs::OsRng.unwrap_err();
    let ciphertext_message = block_on(message_encrypt(
        padded_content,
        recipient_addr,
        local_addr,
        session_store,
        identity_store,
        SystemTime::now(),
        &mut rng,
    ))
    .map_err(|e| OutgoingError::Encrypt(format!("{e:?}")))?;

    let (ciphertext_bytes, ciphertext_type) = match ciphertext_message {
        CiphertextMessage::SignalMessage(msg) => {
            (msg.serialized().to_vec(), ENVELOPE_CIPHERTEXT)
        }
        CiphertextMessage::PreKeySignalMessage(msg) => {
            (msg.serialized().to_vec(), ENVELOPE_PREKEY_BUNDLE)
        }
        other => {
            return Err(OutgoingError::UnsupportedCiphertextType(
                format!("{:?}", other.message_type()),
            ));
        }
    };

    if std::env::var("XSCDEBUG_DUMP").is_ok() {
        let _ = dump_hex(
            &format!(
                "Ciphertext (envelope type={}) for {}/{}",
                ciphertext_type,
                recipient_addr.name(),
                u32::from(recipient_addr.device_id()),
            ),
            timestamp_ms,
            &ciphertext_bytes,
        );
    }

    Ok(EncryptedMessage {
        ciphertext_bytes,
        ciphertext_type,
        destination_device_id: u32::from(recipient_addr.device_id()),
        destination_registration_id: dest_reg_id,
        timestamp_ms,
    })
}

// ---- Generic encrypt over abstract stores (testable without pddb) ----------

pub(crate) fn build_encrypted_message_with_stores<S, I>(
    plaintext_body: &str,
    timestamp_ms: u64,
    recipient_addr: &ProtocolAddress,
    local_addr: &ProtocolAddress,
    session_store: &mut S,
    identity_store: &mut I,
) -> Result<EncryptedMessage, OutgoingError>
where
    S: SessionStore,
    I: IdentityKeyStore,
{
    let padded = build_padded_data_message_content(plaintext_body, timestamp_ms);
    encrypt_padded_for_recipient(
        &padded,
        timestamp_ms,
        recipient_addr,
        local_addr,
        session_store,
        identity_store,
    )
}

// ---- Production wrapper: opens pddb stores, reads local account ------------

pub(crate) fn build_encrypted_message(
    plaintext_body: &str,
    timestamp_ms: u64,
    recipient_addr: &ProtocolAddress,
) -> Result<EncryptedMessage, OutgoingError> {
    let local_addr = local_protocol_address()?;

    // Same multi-handle pattern as receive's dispatch_envelope (Task 7 perf gap;
    // not addressed here).
    let pddb_id = pddb::Pddb::new(); pddb_id.try_mount();
    let pddb_ses = pddb::Pddb::new(); pddb_ses.try_mount();

    let mut identity_store = PddbIdentityStore::new(pddb_id, ACCOUNT_DICT, IDENTITY_DICT);
    let mut session_store = PddbSessionStore::new(pddb_ses, SESSION_DICT);

    build_encrypted_message_with_stores(
        plaintext_body,
        timestamp_ms,
        recipient_addr,
        &local_addr,
        &mut session_store,
        &mut identity_store,
    )
}

// ---- Local account → ProtocolAddress ---------------------------------------

fn local_protocol_address() -> Result<ProtocolAddress, OutgoingError> {
    let pddb = pddb::Pddb::new();
    pddb.try_mount();

    let aci = pddb_get_string(&pddb, ACCOUNT_DICT, ACI_SERVICE_ID_KEY)
        .ok_or_else(|| OutgoingError::NoLocalAccount("aci.service_id missing".into()))?;
    let dev_str = pddb_get_string(&pddb, ACCOUNT_DICT, DEVICE_ID_KEY)
        .ok_or_else(|| OutgoingError::NoLocalAccount("device_id missing".into()))?;
    let dev_id: u32 = dev_str.parse()
        .map_err(|e| OutgoingError::NoLocalAccount(format!("device_id parse: {e}")))?;
    if dev_id == 0 || dev_id > 127 {
        return Err(OutgoingError::BadDeviceId(dev_id));
    }
    let dev = DeviceId::new(dev_id as u8)
        .map_err(|e| OutgoingError::NoLocalAccount(format!("DeviceId: {e:?}")))?;
    Ok(ProtocolAddress::new(aci, dev))
}

// ---- Recipient persistence (V1: most-recent-sender) ------------------------

/// Persist the most recent sender's UUID + device_id as the default outgoing
/// recipient. Called by the receive path after a successful DataMessage
/// delivery so that the user's reply has somewhere to go.
pub fn set_current_recipient(remote_addr: &ProtocolAddress) -> Result<(), OutgoingError> {
    let pddb = pddb::Pddb::new();
    pddb.try_mount();
    let payload = format!(
        "{{\"uuid\":\"{}\",\"device_id\":{}}}",
        remote_addr.name(),
        u32::from(remote_addr.device_id()),
    );
    pddb.delete_key(DIALOGUE_DICT, DEFAULT_PEER_KEY, None).ok();
    let mut h = pddb.get(DIALOGUE_DICT, DEFAULT_PEER_KEY, None, true, true, None, None::<fn()>)
        .map_err(|e| OutgoingError::Pddb(format!("get: {e}")))?;
    h.write_all(payload.as_bytes())
        .map_err(|e| OutgoingError::Pddb(format!("write: {e}")))?;
    pddb.sync().ok();
    Ok(())
}

/// Read the most recent sender as a ProtocolAddress, or `NoRecipient` if no
/// one has messaged us yet.
pub fn current_recipient() -> Result<ProtocolAddress, OutgoingError> {
    let pddb = pddb::Pddb::new();
    pddb.try_mount();
    let raw = pddb_get_string(&pddb, DIALOGUE_DICT, DEFAULT_PEER_KEY)
        .ok_or(OutgoingError::NoRecipient)?;
    parse_peer_json(&raw)
}

/// Pre-seed the default outgoing recipient from environment variables.
///
/// Reads `XSC_DEMO_PEER_UUID` (required, hyphenated UUID string) and
/// `XSC_DEMO_PEER_DEVICE_ID` (optional, default 1). If both parse and a
/// recipient is not already persisted in PDDB, calls
/// [`set_current_recipient`] with the parsed address so a subsequent
/// `SigChat::post()` has somewhere to send before any inbound DataMessage
/// has populated the field via the V1 most-recent-sender mechanism.
///
/// Behavior:
/// - `XSC_DEMO_PEER_UUID` unset → no-op, falls back to the V1 mechanism.
/// - Invalid UUID format → log warning, no-op.
/// - `XSC_DEMO_PEER_DEVICE_ID` unset → defaults to `1` (typical primary).
/// - `XSC_DEMO_PEER_DEVICE_ID` invalid (non-numeric, 0, or >127) →
///   log warning, no-op.
/// - A recipient already persisted in PDDB → the seed is skipped to
///   preserve any V1-captured peer; warn so the operator notices.
///
/// Returns `Ok(true)` if the seed was applied, `Ok(false)` if it was a
/// no-op for any non-error reason (env unset, recipient already present).
pub fn seed_demo_recipient_from_env() -> Result<bool, OutgoingError> {
    let uuid_env = match std::env::var("XSC_DEMO_PEER_UUID") {
        Ok(s) if !s.is_empty() => s,
        _ => return Ok(false),
    };
    let dev_env = std::env::var("XSC_DEMO_PEER_DEVICE_ID").ok();

    let addr = match parse_demo_peer(&uuid_env, dev_env.as_deref()) {
        Ok(addr) => addr,
        Err(reason) => {
            log::warn!("XSC_DEMO_PEER_UUID/_DEVICE_ID rejected: {reason} — skipping seed");
            return Ok(false);
        }
    };

    if current_recipient().is_ok() {
        log::warn!("XSC_DEMO_PEER_UUID set but a recipient is already persisted; skipping seed to preserve V1 state");
        return Ok(false);
    }

    set_current_recipient(&addr)?;
    log::info!(
        "seeded demo recipient: uuid={} device_id={}",
        addr.name(),
        u32::from(addr.device_id()),
    );
    Ok(true)
}

/// Pure-function helper: validate a UUID + device_id and produce a
/// `ProtocolAddress`. `device_id_str` of `None` defaults to 1. Returns
/// a human-readable reason on rejection so callers can log it.
///
/// Hyphenated UUID format is required (36 chars, hyphens at positions
/// 8/13/18/23, ASCII hex elsewhere). DeviceId must be 1..=127.
fn parse_demo_peer(uuid: &str, device_id_str: Option<&str>) -> Result<ProtocolAddress, String> {
    if uuid.len() != 36
        || !uuid.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
    {
        return Err(format!("not a valid hyphenated UUID: {uuid:?}"));
    }
    let device_id: u32 = match device_id_str {
        Some(s) => s.parse().map_err(|e| format!("device_id parse: {e}"))?,
        None => 1,
    };
    if device_id == 0 || device_id > 127 {
        return Err(format!("device_id out of range (1..=127): {device_id}"));
    }
    let dev = DeviceId::new(device_id as u8).map_err(|e| format!("DeviceId: {e:?}"))?;
    Ok(ProtocolAddress::new(uuid.to_string(), dev))
}

fn parse_peer_json(s: &str) -> Result<ProtocolAddress, OutgoingError> {
    // Minimal JSON: {"uuid":"...","device_id":N}
    // Avoid pulling serde_json into outgoing.rs's compile graph for one read.
    let uuid = extract_string_field(s, "uuid")
        .ok_or_else(|| OutgoingError::Pddb("peer json: uuid missing".into()))?;
    let dev_str = extract_number_field(s, "device_id")
        .ok_or_else(|| OutgoingError::Pddb("peer json: device_id missing".into()))?;
    let dev_id: u32 = dev_str.parse()
        .map_err(|e| OutgoingError::Pddb(format!("peer json: device_id parse: {e}")))?;
    if dev_id == 0 || dev_id > 127 {
        return Err(OutgoingError::BadDeviceId(dev_id));
    }
    let dev = DeviceId::new(dev_id as u8)
        .map_err(|e| OutgoingError::Pddb(format!("DeviceId: {e:?}")))?;
    Ok(ProtocolAddress::new(uuid, dev))
}

fn extract_string_field(s: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = s.find(&needle)? + needle.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_number_field(s: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let start = s.find(&needle)? + needle.len();
    let rest = &s[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    Some(rest[..end].to_string())
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

// ---- Tests ------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::convert::TryFrom;
    use libsignal_protocol::{
        GenericSignedPreKey,
        IdentityKeyPair, KeyPair, kem,
        InMemSignalProtocolStore, PreKeyBundle, PreKeyRecord,
        SignedPreKeyRecord, KyberPreKeyRecord,
        Timestamp,
        IdentityKeyStore as _,
        KyberPreKeyStore as _,
        PreKeyStore as _,
        SignedPreKeyStore as _,
        message_decrypt,
        process_prekey_bundle,
    };
    use rand::Rng;
    use rand::rngs::OsRng;

    fn fresh_store() -> InMemSignalProtocolStore {
        let mut rng = OsRng.unwrap_err();
        let identity_key = IdentityKeyPair::generate(&mut rng);
        // Valid registration IDs fit in 14 bits.
        let registration_id: u32 = rng.random_range(1..16383);
        InMemSignalProtocolStore::new(identity_key, registration_id).unwrap()
    }

    /// Build a PreKeyBundle for `store`, persisting the corresponding records.
    /// Mirrors libsignal's tests/support::create_pre_key_bundle.
    fn make_bundle(store: &mut InMemSignalProtocolStore, device_id: DeviceId)
        -> PreKeyBundle
    {
        let mut rng = OsRng.unwrap_err();
        let pre_key_pair = KeyPair::generate(&mut rng);
        let signed_pre_key_pair = KeyPair::generate(&mut rng);
        let kyber_pre_key_pair = kem::KeyPair::generate(kem::KeyType::Kyber1024, &mut rng);

        let identity_key_pair = block_on(store.get_identity_key_pair()).unwrap();

        let signed_pub = signed_pre_key_pair.public_key.serialize();
        let signed_sig = identity_key_pair.private_key()
            .calculate_signature(&signed_pub, &mut rng).unwrap();

        let kyber_pub = kyber_pre_key_pair.public_key.serialize();
        let kyber_sig = identity_key_pair.private_key()
            .calculate_signature(&kyber_pub, &mut rng).unwrap();

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
        ).unwrap();

        block_on(store.save_pre_key(
            pre_key_id.into(),
            &PreKeyRecord::new(pre_key_id.into(), &pre_key_pair),
        )).unwrap();

        block_on(store.save_signed_pre_key(
            signed_pre_key_id.into(),
            &SignedPreKeyRecord::new(
                signed_pre_key_id.into(),
                Timestamp::from_epoch_millis(42),
                &signed_pre_key_pair,
                &signed_sig,
            ),
        )).unwrap();

        block_on(store.save_kyber_pre_key(
            kyber_pre_key_id.into(),
            &KyberPreKeyRecord::new(
                kyber_pre_key_id.into(),
                Timestamp::from_epoch_millis(43),
                &kyber_pre_key_pair,
                &kyber_sig,
            ),
        )).unwrap();

        bundle
    }

    fn strip_signal_padding(mut plaintext: Vec<u8>) -> Vec<u8> {
        while plaintext.last() == Some(&0x00) { plaintext.pop(); }
        if plaintext.last() == Some(&0x80) { plaintext.pop(); }
        plaintext
    }

    #[test]
    fn signal_pad_appends_marker_and_pads_to_160() {
        let mut v = vec![0xAA; 50];
        signal_pad(&mut v);
        assert_eq!(v.len(), 160);
        assert_eq!(v[50], 0x80);
        for byte in &v[51..] {
            assert_eq!(*byte, 0x00);
        }
    }

    #[test]
    fn signal_pad_at_exact_multiple_pads_to_next() {
        let mut v = vec![0xCC; 160];
        signal_pad(&mut v);
        assert_eq!(v.len(), 320);
        assert_eq!(v[160], 0x80);
    }

    #[test]
    fn pad_then_strip_round_trips() {
        let original = vec![1, 2, 3, 4, 5, 0x80, 6, 7];
        let mut padded = original.clone();
        signal_pad(&mut padded);
        assert_ne!(padded.len(), original.len());
        let recovered = strip_signal_padding(padded);
        assert_eq!(recovered, original);
    }

    #[test]
    fn parse_peer_json_minimal() {
        let s = r#"{"uuid":"abcd-1234","device_id":2}"#;
        let addr = parse_peer_json(s).unwrap();
        assert_eq!(addr.name(), "abcd-1234");
        assert_eq!(u32::from(addr.device_id()), 2);
    }

    #[test]
    fn parse_peer_json_rejects_bad_device_id() {
        let s = r#"{"uuid":"x","device_id":0}"#;
        assert!(matches!(parse_peer_json(s), Err(OutgoingError::BadDeviceId(0))));
        let s = r#"{"uuid":"x","device_id":999}"#;
        assert!(matches!(parse_peer_json(s), Err(OutgoingError::BadDeviceId(999))));
    }

    /// End-to-end: Alice encrypts with build_encrypted_message_with_stores,
    /// Bob decrypts with libsignal's message_decrypt, padding is stripped,
    /// proto is decoded, body and timestamp match the input.
    ///
    /// This covers the full content→pad→encrypt→decrypt→strip→proto chain
    /// without touching pddb or the network.
    #[test]
    fn encrypt_roundtrip_first_message_is_prekey_bundle() {
        let mut alice_store = fresh_store();
        let mut bob_store = fresh_store();

        let alice_addr = ProtocolAddress::new(
            "alice-uuid".to_string(),
            DeviceId::new(1).unwrap(),
        );
        let bob_addr = ProtocolAddress::new(
            "bob-uuid".to_string(),
            DeviceId::new(2).unwrap(),
        );

        // Bob publishes a pre-key bundle; Alice processes it to bootstrap the
        // outbound session.
        let bob_bundle = make_bundle(&mut bob_store, DeviceId::new(2).unwrap());
        let mut rng = OsRng.unwrap_err();
        block_on(process_prekey_bundle(
            &bob_addr,
            &mut alice_store.session_store,
            &mut alice_store.identity_store,
            &bob_bundle,
            SystemTime::now(),
            &mut rng,
        )).unwrap();

        // Alice encrypts.
        let plaintext_body = "hello world";
        let ts: u64 = 1_700_000_000_000;
        let enc = build_encrypted_message_with_stores(
            plaintext_body,
            ts,
            &bob_addr,
            &alice_addr,
            &mut alice_store.session_store,
            &mut alice_store.identity_store,
        ).unwrap();

        // First message after process_prekey_bundle is always PreKeySignalMessage.
        assert_eq!(enc.ciphertext_type, ENVELOPE_PREKEY_BUNDLE);
        assert_eq!(enc.destination_device_id, 2);
        assert_eq!(enc.timestamp_ms, ts);
        assert!(!enc.ciphertext_bytes.is_empty());

        // Bob reconstructs the CiphertextMessage and decrypts.
        let bob_view = libsignal_protocol::PreKeySignalMessage::try_from(
            enc.ciphertext_bytes.as_slice()
        ).unwrap();
        let bob_view = CiphertextMessage::PreKeySignalMessage(bob_view);

        let decrypted_padded = block_on(message_decrypt(
            &bob_view,
            &alice_addr,
            &bob_addr,
            &mut bob_store.session_store,
            &mut bob_store.identity_store,
            &mut bob_store.pre_key_store,
            &bob_store.signed_pre_key_store,
            &mut bob_store.kyber_pre_key_store,
            &mut rng,
        )).unwrap();

        let stripped = strip_signal_padding(decrypted_padded);
        let content = ContentProto::decode(stripped.as_slice()).unwrap();
        let dm = content.data_message.expect("DataMessage present");
        assert_eq!(dm.body.as_deref(), Some(plaintext_body));
        assert_eq!(dm.timestamp, Some(ts));
    }

    /// Second message in the same session must be CIPHERTEXT (SignalMessage),
    /// not PREKEY_BUNDLE — confirms the session state is being saved.
    #[test]
    fn encrypt_second_message_is_ciphertext() {
        let mut alice_store = fresh_store();
        let mut bob_store = fresh_store();

        let alice_addr = ProtocolAddress::new(
            "alice-uuid".to_string(),
            DeviceId::new(1).unwrap(),
        );
        let bob_addr = ProtocolAddress::new(
            "bob-uuid".to_string(),
            DeviceId::new(2).unwrap(),
        );
        let bob_bundle = make_bundle(&mut bob_store, DeviceId::new(2).unwrap());
        let mut rng = OsRng.unwrap_err();
        block_on(process_prekey_bundle(
            &bob_addr,
            &mut alice_store.session_store,
            &mut alice_store.identity_store,
            &bob_bundle,
            SystemTime::now(),
            &mut rng,
        )).unwrap();

        // First send — primes the session, produces PreKeySignalMessage. Bob
        // must decrypt it so his side acknowledges the session, otherwise
        // every Alice→Bob message stays a PreKeySignalMessage.
        let enc1 = build_encrypted_message_with_stores(
            "first", 1, &bob_addr, &alice_addr,
            &mut alice_store.session_store, &mut alice_store.identity_store,
        ).unwrap();
        assert_eq!(enc1.ciphertext_type, ENVELOPE_PREKEY_BUNDLE);
        let pkm = libsignal_protocol::PreKeySignalMessage::try_from(
            enc1.ciphertext_bytes.as_slice()
        ).unwrap();
        let _ = block_on(message_decrypt(
            &CiphertextMessage::PreKeySignalMessage(pkm),
            &alice_addr,
            &bob_addr,
            &mut bob_store.session_store,
            &mut bob_store.identity_store,
            &mut bob_store.pre_key_store,
            &bob_store.signed_pre_key_store,
            &mut bob_store.kyber_pre_key_store,
            &mut rng,
        )).unwrap();
        // Bob now replies so Alice's side of the session also advances.
        let bob_alice_addr = ProtocolAddress::new(
            "alice-uuid".to_string(),
            DeviceId::new(1).unwrap(),
        );
        let enc_bob = build_encrypted_message_with_stores(
            "ack", 2, &bob_alice_addr, &bob_addr,
            &mut bob_store.session_store, &mut bob_store.identity_store,
        ).unwrap();
        let sm = libsignal_protocol::SignalMessage::try_from(
            enc_bob.ciphertext_bytes.as_slice()
        ).unwrap();
        let _ = block_on(message_decrypt(
            &CiphertextMessage::SignalMessage(sm),
            &bob_addr,
            &alice_addr,
            &mut alice_store.session_store,
            &mut alice_store.identity_store,
            &mut alice_store.pre_key_store,
            &alice_store.signed_pre_key_store,
            &mut alice_store.kyber_pre_key_store,
            &mut rng,
        )).unwrap();

        // Second send from Alice — must now be SignalMessage (CIPHERTEXT).
        let enc2 = build_encrypted_message_with_stores(
            "second", 3, &bob_addr, &alice_addr,
            &mut alice_store.session_store, &mut alice_store.identity_store,
        ).unwrap();
        assert_eq!(enc2.ciphertext_type, ENVELOPE_CIPHERTEXT);
    }

    #[test]
    fn no_session_returns_no_session_error() {
        let mut alice_store = fresh_store();
        let alice_addr = ProtocolAddress::new(
            "alice-uuid".to_string(),
            DeviceId::new(1).unwrap(),
        );
        let bob_addr = ProtocolAddress::new(
            "bob-uuid".to_string(),
            DeviceId::new(2).unwrap(),
        );
        let result = build_encrypted_message_with_stores(
            "no session", 1, &bob_addr, &alice_addr,
            &mut alice_store.session_store, &mut alice_store.identity_store,
        );
        assert!(matches!(result, Err(OutgoingError::NoSession)));
    }

    // ---- parse_demo_peer ----------------------------------------------------

    const VALID_UUID: &str = "00000000-0000-4000-8000-000000000001";

    #[test]
    fn parse_demo_peer_default_device_is_1() {
        let addr = parse_demo_peer(VALID_UUID, None).unwrap();
        assert_eq!(addr.name(), VALID_UUID);
        assert_eq!(u32::from(addr.device_id()), 1);
    }

    #[test]
    fn parse_demo_peer_explicit_device() {
        let addr = parse_demo_peer(VALID_UUID, Some("2")).unwrap();
        assert_eq!(u32::from(addr.device_id()), 2);
    }

    #[test]
    fn parse_demo_peer_rejects_short_uuid() {
        let err = parse_demo_peer("00000000", None).unwrap_err();
        assert!(err.contains("UUID"), "expected UUID error, got: {err}");
    }

    #[test]
    fn parse_demo_peer_rejects_misplaced_hyphens() {
        // Right length (36) but hyphens in wrong positions.
        let bad = "000000000-0000-4000-8000-000000000001";
        assert_eq!(bad.len(), 37, "test fixture wrong length");
        let bad = &bad[..36];
        let err = parse_demo_peer(bad, None).unwrap_err();
        assert!(err.contains("UUID"));
    }

    #[test]
    fn parse_demo_peer_rejects_non_hex() {
        let bad = "zz000000-0000-4000-8000-000000000001";
        let err = parse_demo_peer(bad, None).unwrap_err();
        assert!(err.contains("UUID"));
    }

    #[test]
    fn parse_demo_peer_rejects_device_id_zero() {
        let err = parse_demo_peer(VALID_UUID, Some("0")).unwrap_err();
        assert!(err.contains("out of range"), "got: {err}");
    }

    #[test]
    fn parse_demo_peer_rejects_device_id_too_large() {
        let err = parse_demo_peer(VALID_UUID, Some("128")).unwrap_err();
        assert!(err.contains("out of range"), "got: {err}");
    }

    #[test]
    fn parse_demo_peer_rejects_non_numeric_device_id() {
        let err = parse_demo_peer(VALID_UUID, Some("abc")).unwrap_err();
        assert!(err.contains("parse"), "got: {err}");
    }
}
