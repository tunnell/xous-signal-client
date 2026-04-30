// Prekey generation for the secondary-device link flow.
// Produces the four prekey JSON objects required by PUT /v1/devices/link,
// and retains the private-key records so they can be saved to the pddb
// store after a successful link (required for subsequent message decryption).
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use futures::executor::block_on;
use libsignal_protocol::{
    GenericSignedPreKey, KeyPair as DjbKeyPair, KyberPreKeyId, KyberPreKeyRecord,
    KyberPreKeyStore, PreKeyId, PreKeyRecord, PreKeyStore, PrivateKey, SignedPreKeyId,
    SignedPreKeyRecord, SignedPreKeyStore, Timestamp, kem,
};
use rand::{RngCore, TryRngCore as _, rngs::OsRng};
use std::io::{Error, ErrorKind, Read, Write};

use crate::manager::stores::{PddbKyberPreKeyStore, PddbPreKeyStore, PddbSignedPreKeyStore};

/// Medium.MAX_VALUE (2^24 - 1) — the upper bound for Signal's prekey IDs.
const MEDIUM_MAX: u32 = 0x00FF_FFFF;

const SIGNED_PREKEY_DICT: &str = "sigchat.signed_prekey";
const KYBER_PREKEY_DICT: &str = "sigchat.kyber_prekey";
const PREKEY_DICT: &str = "sigchat.prekey";

const ACCOUNT_DICT: &str = "sigchat.account";
/// PDDB key under [`ACCOUNT_DICT`] holding the ACI one-time-prekey ID
/// counter as a decimal string. Persistent so replenishment doesn't
/// reuse IDs across runs. See ADR 0013.
const ACI_NEXT_PREKEY_ID_KEY: &str = "aci.next_prekey_id";

/// Threshold below which the orchestrator uploads a fresh batch.
/// Matches `libsignal-service-rs::pre_keys::PRE_KEY_MINIMUM`.
pub const PRE_KEY_MINIMUM: u32 = 10;
/// Number of one-time EC prekeys generated per replenishment call.
/// Matches `libsignal-service-rs::pre_keys::PRE_KEY_BATCH_SIZE` and
/// is the per-call cap enforced server-side by Signal-Server's
/// `SetKeysRequest` validation (max 100).
pub const PRE_KEY_BATCH_SIZE: u32 = 100;

pub struct SignedPreKeyJson {
    pub key_id: u32,
    pub public_key_b64url: String,
    pub signature_b64url: String,
}

pub struct KyberPreKeyJson {
    pub key_id: u32,
    pub public_key_b64url: String,
    pub signature_b64url: String,
}

pub struct Prekeys {
    pub aci_signed: SignedPreKeyJson,
    pub pni_signed: SignedPreKeyJson,
    pub aci_kyber_last_resort: KyberPreKeyJson,
    pub pni_kyber_last_resort: KyberPreKeyJson,
    // Private records — must be saved to pddb after successful link.
    pub aci_signed_record: SignedPreKeyRecord,
    pub pni_signed_record: SignedPreKeyRecord,
    pub aci_kyber_record: KyberPreKeyRecord,
    pub pni_kyber_record: KyberPreKeyRecord,
}

/// Generate ACI+PNI signed and Kyber last-resort prekeys, signed by the
/// corresponding identity private key. All four JSON values are encoded as
/// standard no-padding base64 per the Signal spec. The private-key records
/// are retained in the returned struct — call `save_to_pddb` after a
/// successful link to persist them.
pub fn generate_prekeys(
    aci_identity_private: &PrivateKey,
    pni_identity_private: &PrivateKey,
) -> Result<Prekeys, Error> {
    let (aci_signed, aci_signed_record) = generate_signed_prekey(aci_identity_private, "aci")?;
    let (pni_signed, pni_signed_record) = generate_signed_prekey(pni_identity_private, "pni")?;
    let (aci_kyber_last_resort, aci_kyber_record) =
        generate_kyber_last_resort(aci_identity_private, "aci")?;
    let (pni_kyber_last_resort, pni_kyber_record) =
        generate_kyber_last_resort(pni_identity_private, "pni")?;
    Ok(Prekeys {
        aci_signed,
        pni_signed,
        aci_kyber_last_resort,
        pni_kyber_last_resort,
        aci_signed_record,
        pni_signed_record,
        aci_kyber_record,
        pni_kyber_record,
    })
}

/// Persist prekey private-key records to the pddb stores used by main_ws.
/// Must be called after the link REST call returns 200 — calling before that
/// risks persisting keys that were never uploaded (e.g. if the link fails).
pub fn save_to_pddb(prekeys: &Prekeys) -> Result<(), Error> {
    let pddb_spk = pddb::Pddb::new();
    pddb_spk.try_mount();
    let pddb_kpk = pddb::Pddb::new();
    pddb_kpk.try_mount();

    let mut spk_store = PddbSignedPreKeyStore::new(pddb_spk, SIGNED_PREKEY_DICT);
    let mut kpk_store = PddbKyberPreKeyStore::new(pddb_kpk, KYBER_PREKEY_DICT);

    let aci_spk_id = prekeys.aci_signed_record.id().map_err(map_signal_err)?;
    block_on(spk_store.save_signed_pre_key(aci_spk_id, &prekeys.aci_signed_record))
        .map_err(map_signal_err)?;
    log::info!("prekeys: saved aci signed prekey id={}", u32::from(aci_spk_id));

    let pni_spk_id = prekeys.pni_signed_record.id().map_err(map_signal_err)?;
    block_on(spk_store.save_signed_pre_key(pni_spk_id, &prekeys.pni_signed_record))
        .map_err(map_signal_err)?;
    log::info!("prekeys: saved pni signed prekey id={}", u32::from(pni_spk_id));

    let aci_kpk_id = prekeys.aci_kyber_record.id().map_err(map_signal_err)?;
    block_on(kpk_store.save_kyber_pre_key(aci_kpk_id, &prekeys.aci_kyber_record))
        .map_err(map_signal_err)?;
    log::info!("prekeys: saved aci kyber last-resort id={}", u32::from(aci_kpk_id));

    let pni_kpk_id = prekeys.pni_kyber_record.id().map_err(map_signal_err)?;
    block_on(kpk_store.save_kyber_pre_key(pni_kpk_id, &prekeys.pni_kyber_record))
        .map_err(map_signal_err)?;
    log::info!("prekeys: saved pni kyber last-resort id={}", u32::from(pni_kpk_id));

    Ok(())
}

fn map_signal_err(e: libsignal_protocol::SignalProtocolError) -> Error {
    Error::new(ErrorKind::Other, format!("libsignal: {e:?}"))
}

fn random_prekey_id() -> Result<u32, Error> {
    let mut rng = OsRng.unwrap_err();
    Ok((rng.next_u32() % MEDIUM_MAX) + 1)
}

/// JSON shape of a single one-time EC prekey on the `PUT /v2/keys`
/// upload wire (Signal-Server's `ECPreKey` record). One-time prekeys
/// are NOT signed — the signature lives on the SignedPreKey alone.
pub struct OneTimePreKeyJson {
    pub key_id: u32,
    pub public_key_b64url: String,
}

/// Result of a successful `generate_one_time_prekeys` call: the
/// JSON payloads suitable for the upload body, plus the persisted
/// records (already saved to PDDB) so the caller can log how many
/// new keys are now staged for inbound use.
pub struct OneTimePreKeyBatch {
    pub json: Vec<OneTimePreKeyJson>,
    pub count: u32,
}

/// Generate `count` fresh X25519 one-time EC prekeys, persist each
/// to the `sigchat.prekey` PDDB dict via [`PddbPreKeyStore`], and
/// advance the persisted ACI counter. IDs wrap modulo `MEDIUM_MAX`
/// so id 0 is never used (Signal protocol rule).
///
/// Persistence happens BEFORE the caller uploads — if the upload
/// later fails, the records remain in PDDB and the next replenish
/// cycle will overlap-but-not-collide with new IDs from an advanced
/// counter. Locally-stored prekeys the server never accepted are
/// harmless: they sit unused under their ID and would only be
/// looked up if a peer ever tried to use them, which can't happen
/// because the server never advertised them in any prekey bundle.
pub fn generate_one_time_prekeys(count: u32) -> Result<OneTimePreKeyBatch, Error> {
    if count == 0 {
        return Ok(OneTimePreKeyBatch { json: Vec::new(), count: 0 });
    }

    let pddb = pddb::Pddb::new();
    pddb.try_mount();

    let start_id = read_or_init_prekey_counter(&pddb)?;
    let mut store = PddbPreKeyStore::new(pddb::Pddb::new(), PREKEY_DICT);
    let mut rng = OsRng.unwrap_err();

    let mut json = Vec::with_capacity(count as usize);
    let mut next = start_id;
    for _ in 0..count {
        let id = next;
        // Wrap to keep the next ID in (0, MEDIUM_MAX].
        next = if next >= MEDIUM_MAX { 1 } else { next + 1 };

        let kp = DjbKeyPair::generate(&mut rng);
        let public_serialized = kp.public_key.serialize();
        let record = PreKeyRecord::new(PreKeyId::from(id), &kp);

        block_on(store.save_pre_key(PreKeyId::from(id), &record)).map_err(map_signal_err)?;

        json.push(OneTimePreKeyJson {
            key_id: id,
            public_key_b64url: STANDARD_NO_PAD.encode(&public_serialized),
        });
    }

    write_prekey_counter(&pddb, next)?;
    log::info!(
        "prekeys: generated {} one-time EC prekeys (id range {}..={}, next={})",
        count, start_id, json.last().map(|j| j.key_id).unwrap_or(start_id), next,
    );
    Ok(OneTimePreKeyBatch { json, count })
}

/// Read the persistent ACI prekey-id counter. On first call (key
/// absent), seed it with a small random offset in [1, 1000] so peers
/// can't trivially correlate ID ranges across freshly-installed
/// devices, and persist the seed.
fn read_or_init_prekey_counter(pddb: &pddb::Pddb) -> Result<u32, Error> {
    if let Some(s) = pddb_get_string(pddb, ACCOUNT_DICT, ACI_NEXT_PREKEY_ID_KEY) {
        if let Ok(v) = s.trim().parse::<u32>() {
            if v >= 1 && v <= MEDIUM_MAX {
                return Ok(v);
            }
            log::warn!(
                "prekeys: aci.next_prekey_id={} out of range, reseeding", v
            );
        } else {
            log::warn!("prekeys: aci.next_prekey_id parse failed, reseeding");
        }
    }
    let mut rng = OsRng.unwrap_err();
    let seed = (rng.next_u32() % 1000) + 1;
    write_prekey_counter(pddb, seed)?;
    Ok(seed)
}

fn write_prekey_counter(pddb: &pddb::Pddb, next: u32) -> Result<(), Error> {
    let s = format!("{}", next);
    pddb.delete_key(ACCOUNT_DICT, ACI_NEXT_PREKEY_ID_KEY, None).ok();
    let mut h = pddb
        .get(ACCOUNT_DICT, ACI_NEXT_PREKEY_ID_KEY, None, true, true, None, None::<fn()>)
        .map_err(|e| Error::new(ErrorKind::Other, format!("counter get: {e}")))?;
    h.write_all(s.as_bytes())
        .map_err(|e| Error::new(ErrorKind::Other, format!("counter write: {e}")))?;
    pddb.sync().ok();
    Ok(())
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

/// Pure-function helper for the wrap-around test: given a starting
/// counter and the number of keys generated, return the value the
/// counter should be advanced to. Mirrors the inline arithmetic in
/// [`generate_one_time_prekeys`] without the PDDB / RNG side effects.
#[cfg(test)]
fn advance_prekey_counter(start: u32, count: u32) -> u32 {
    let mut next = start;
    for _ in 0..count {
        next = if next >= MEDIUM_MAX { 1 } else { next + 1 };
    }
    next
}

/// X25519 signed prekey: fresh Curve25519 keypair whose serialized public key
/// is signed (Ed25519 on Curve25519) by the identity private key.
fn generate_signed_prekey(
    identity_private: &PrivateKey,
    label: &str,
) -> Result<(SignedPreKeyJson, SignedPreKeyRecord), Error> {
    let mut rng = OsRng.unwrap_err();
    let key_id = random_prekey_id()?;
    let keypair = DjbKeyPair::generate(&mut rng);
    let public_serialized = keypair.public_key.serialize(); // 33 bytes: 0x05 prefix + 32 key
    let signature = identity_private
        .calculate_signature(&public_serialized, &mut rng)
        .map_err(|e| {
            log::error!("{label} signed prekey signing failed: {e:?}");
            Error::new(ErrorKind::Other, "signed prekey signing failed")
        })?;

    let identity_public = identity_private.public_key().map_err(|e| {
        log::error!("{label} identity public derivation failed: {e:?}");
        Error::new(ErrorKind::Other, "identity public key derivation failed")
    })?;
    if !identity_public.verify_signature(&public_serialized, &signature) {
        log::error!("{label} signed prekey self-verification failed — aborting link");
        return Err(Error::new(
            ErrorKind::InvalidData,
            "signed prekey self-verification failed",
        ));
    }

    let record = SignedPreKeyRecord::new(
        SignedPreKeyId::from(key_id),
        Timestamp::from_epoch_millis(0),
        &keypair,
        &signature,
    );

    let json = SignedPreKeyJson {
        key_id,
        public_key_b64url: STANDARD_NO_PAD.encode(&public_serialized),
        signature_b64url: STANDARD_NO_PAD.encode(&signature),
    };
    Ok((json, record))
}

/// Kyber1024 last-resort prekey: fresh KEM keypair whose serialized public key
/// is signed (Ed25519) by the identity private key.
fn generate_kyber_last_resort(
    identity_private: &PrivateKey,
    label: &str,
) -> Result<(KyberPreKeyJson, KyberPreKeyRecord), Error> {
    let key_id = random_prekey_id()?;
    let record = KyberPreKeyRecord::generate(
        kem::KeyType::Kyber1024,
        KyberPreKeyId::from(key_id),
        identity_private,
    )
    .map_err(|e| {
        log::error!("{label} kyber last-resort generation failed: {e:?}");
        Error::new(ErrorKind::Other, "kyber last-resort generation failed")
    })?;

    let public_serialized = record.public_key().map_err(|e| {
        log::error!("{label} kyber public_key() failed: {e:?}");
        Error::new(ErrorKind::Other, "kyber public_key() failed")
    })?.serialize();
    let signature = record.signature().map_err(|e| {
        log::error!("{label} kyber signature() failed: {e:?}");
        Error::new(ErrorKind::Other, "kyber signature() failed")
    })?.to_vec();

    let identity_public = identity_private.public_key().map_err(|e| {
        log::error!("{label} identity public derivation failed: {e:?}");
        Error::new(ErrorKind::Other, "identity public key derivation failed")
    })?;
    if !identity_public.verify_signature(&public_serialized, &signature) {
        log::error!("{label} kyber prekey self-verification failed — aborting link");
        return Err(Error::new(
            ErrorKind::InvalidData,
            "kyber prekey self-verification failed",
        ));
    }

    let json = KyberPreKeyJson {
        key_id,
        public_key_b64url: STANDARD_NO_PAD.encode(&public_serialized),
        signature_b64url: STANDARD_NO_PAD.encode(&signature),
    };
    Ok((json, record))
}

#[cfg(test)]
mod tests {
    use super::*;
    use libsignal_protocol::IdentityKeyPair;

    #[test]
    fn generate_prekeys_emits_four_with_expected_shapes() {
        let mut rng = OsRng.unwrap_err();
        let aci = IdentityKeyPair::generate(&mut rng);
        let pni = IdentityKeyPair::generate(&mut rng);

        let prekeys = generate_prekeys(aci.private_key(), pni.private_key())
            .expect("generate_prekeys should succeed with fresh identity keys");

        // X25519 serialized public key = 33 bytes -> 44 STANDARD_NO_PAD chars.
        // Ed25519 signature = 64 bytes -> 86 STANDARD_NO_PAD chars.
        assert_eq!(prekeys.aci_signed.public_key_b64url.len(), 44);
        assert_eq!(prekeys.aci_signed.signature_b64url.len(), 86);
        assert_eq!(prekeys.pni_signed.public_key_b64url.len(), 44);
        assert_eq!(prekeys.pni_signed.signature_b64url.len(), 86);

        // Kyber1024 serialized public key is much larger — just assert non-empty.
        assert!(!prekeys.aci_kyber_last_resort.public_key_b64url.is_empty());
        assert_eq!(prekeys.aci_kyber_last_resort.signature_b64url.len(), 86);
        assert!(!prekeys.pni_kyber_last_resort.public_key_b64url.is_empty());
        assert_eq!(prekeys.pni_kyber_last_resort.signature_b64url.len(), 86);

        // Key IDs are within Medium range.
        for id in [
            prekeys.aci_signed.key_id,
            prekeys.pni_signed.key_id,
            prekeys.aci_kyber_last_resort.key_id,
            prekeys.pni_kyber_last_resort.key_id,
        ] {
            assert!(id >= 1 && id <= MEDIUM_MAX);
        }
    }

    #[test]
    fn advance_counter_basic_increment() {
        assert_eq!(advance_prekey_counter(1, 5), 6);
        assert_eq!(advance_prekey_counter(100, 100), 200);
    }

    #[test]
    fn advance_counter_wraps_at_medium_max() {
        // Starting near the top, generating 3 should wrap once.
        let near_top = MEDIUM_MAX - 1;
        // [near_top -> MEDIUM_MAX -> wrap to 1 -> 2]
        assert_eq!(advance_prekey_counter(near_top, 3), 2);
    }

    #[test]
    fn advance_counter_at_max_wraps_immediately() {
        assert_eq!(advance_prekey_counter(MEDIUM_MAX, 1), 1);
        assert_eq!(advance_prekey_counter(MEDIUM_MAX, 2), 2);
    }

    #[test]
    fn advance_counter_zero_count_is_noop() {
        assert_eq!(advance_prekey_counter(42, 0), 42);
    }
}
