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
    KyberPreKeyStore, PrivateKey, SignedPreKeyId, SignedPreKeyRecord, SignedPreKeyStore,
    Timestamp, kem,
};
use rand::{RngCore, TryRngCore as _, rngs::OsRng};
use std::io::{Error, ErrorKind};

use crate::manager::stores::{PddbKyberPreKeyStore, PddbSignedPreKeyStore};

/// Medium.MAX_VALUE (2^24 - 1) — the upper bound for Signal's prekey IDs.
const MEDIUM_MAX: u32 = 0x00FF_FFFF;

const SIGNED_PREKEY_DICT: &str = "sigchat.signed_prekey";
const KYBER_PREKEY_DICT: &str = "sigchat.kyber_prekey";

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
}
