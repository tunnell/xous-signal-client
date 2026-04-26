// Helpers for the secondary-device link REST body that are only used once
// during `account.link()`: random password, unidentified-access-key derivation,
// random registration IDs, and the `accountAttributes` JSON object itself.
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rand::{RngCore, TryRngCore as _, rngs::OsRng};
use serde::Serialize;
use std::io::{Error, ErrorKind};

/// Generate the 18-byte random password used in the Basic-auth header for the
/// link REST call and for subsequent authenticated calls on behalf of this
/// device. Standard (padded) base64 — matches Signal-Android's
/// Base64.encodeWithPadding convention.
pub fn generate_link_password() -> Result<String, Error> {
    let mut bytes = [0u8; 18];
    OsRng.try_fill_bytes(&mut bytes).map_err(|e| {
        log::error!("OsRng fill for password failed: {e:?}");
        Error::new(ErrorKind::Other, "rng failure")
    })?;
    Ok(STANDARD.encode(&bytes))
}

/// Derive the 16-byte unidentifiedAccessKey from the 32-byte profile key.
///
/// Reference: Signal-Android
/// `lib/libsignal-service/src/main/java/org/whispersystems/signalservice/api/crypto/UnidentifiedAccess.java`
/// lines 51-66. Java uses `Cipher.getInstance("AES/GCM/NoPadding")` with
/// `SecretKeySpec(profileKey.serialize(), "AES")` — the profile key is 32
/// bytes so this is **AES-256-GCM**, not AES-128. 12-byte zero nonce, 16
/// zero bytes of plaintext, 128-bit GCM tag; the final `ByteUtil.trim(ct, 16)`
/// takes the first 16 bytes of output (the ciphertext, excluding the tag).
pub fn derive_unidentified_access_key(profile_key: &[u8]) -> Result<[u8; 16], Error> {
    if profile_key.len() != 32 {
        log::error!(
            "profile_key has unexpected length {} (expected 32)",
            profile_key.len()
        );
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "profile_key must be 32 bytes",
        ));
    }
    let key = Key::<Aes256Gcm>::from_slice(profile_key);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&[0u8; 12]);
    let plaintext = [0u8; 16];
    let encrypted = cipher
        .encrypt(
            nonce,
            Payload {
                msg: &plaintext,
                aad: &[],
            },
        )
        .map_err(|e| {
            log::error!("AES-256-GCM encrypt for UAK derivation failed: {e}");
            Error::new(ErrorKind::Other, "UAK AES-GCM failure")
        })?;
    // `encrypted` is [ciphertext(16) || tag(16)]; keep the ciphertext.
    let mut out = [0u8; 16];
    out.copy_from_slice(&encrypted[..16]);
    Ok(out)
}

/// Random registration ID in `[1, 16380]`, matching
/// `KeyHelper.generateRegistrationId(false)`.
pub fn generate_registration_id() -> Result<u16, Error> {
    let mut rng = OsRng.unwrap_err();
    Ok(((rng.next_u32() % 16380) + 1) as u16)
}

#[derive(Serialize)]
pub struct Capabilities {
    pub storage: bool,
    #[serde(rename = "versionedExpirationTimer")]
    pub versioned_expiration_timer: bool,
    #[serde(rename = "attachmentBackfill")]
    pub attachment_backfill: bool,
    pub ssre2: bool,
    pub spqr: bool,
}

#[derive(Serialize)]
pub struct AccountAttributes {
    #[serde(rename = "signalingKey")]
    pub signaling_key: Option<String>,
    #[serde(rename = "registrationId")]
    pub registration_id: u32,
    pub voice: bool,
    pub video: bool,
    #[serde(rename = "fetchesMessages")]
    pub fetches_messages: bool,
    #[serde(rename = "registrationLock")]
    pub registration_lock: Option<String>,
    #[serde(rename = "unidentifiedAccessKey")]
    pub unidentified_access_key: String,
    #[serde(rename = "unrestrictedUnidentifiedAccess")]
    pub unrestricted_unidentified_access: bool,
    #[serde(rename = "discoverableByPhoneNumber")]
    pub discoverable_by_phone_number: bool,
    pub capabilities: Capabilities,
    pub name: String,
    #[serde(rename = "pniRegistrationId")]
    pub pni_registration_id: u32,
    #[serde(rename = "recoveryPassword")]
    pub recovery_password: Option<String>,
}

pub fn build_account_attributes(
    encrypted_device_name_b64: String,
    profile_key: &[u8],
    registration_id: u16,
    pni_registration_id: u16,
) -> Result<AccountAttributes, Error> {
    let uak = derive_unidentified_access_key(profile_key)?;
    Ok(AccountAttributes {
        signaling_key: None,
        registration_id: registration_id as u32,
        voice: true,
        video: true,
        fetches_messages: true,
        registration_lock: None,
        unidentified_access_key: STANDARD.encode(uak),
        unrestricted_unidentified_access: false,
        discoverable_by_phone_number: false,
        capabilities: Capabilities {
            storage: true,
            versioned_expiration_timer: true,
            attachment_backfill: true,
            ssre2: true,
            spqr: true,
        },
        name: encrypted_device_name_b64,
        pni_registration_id: pni_registration_id as u32,
        recovery_password: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_is_24_ascii_chars() {
        let p = generate_link_password().expect("password generation");
        assert_eq!(p.len(), 24);
        assert!(p.is_ascii());
    }

    #[test]
    fn password_is_random_across_calls() {
        let a = generate_link_password().expect("a");
        let b = generate_link_password().expect("b");
        assert_ne!(a, b);
    }

    #[test]
    fn registration_id_in_range() {
        for _ in 0..32 {
            let id = generate_registration_id().expect("reg id");
            assert!(id >= 1 && id <= 16380);
        }
    }

    #[test]
    fn uak_requires_32_byte_profile_key() {
        assert!(derive_unidentified_access_key(&[0u8; 16]).is_err());
        assert!(derive_unidentified_access_key(&[0u8; 32]).is_ok());
    }

    #[test]
    fn uak_zero_profile_key_known_answer() {
        // AES-256-GCM with 32-byte zero key, 12-byte zero nonce, 16 zero plaintext bytes
        // has a stable first-16-bytes-of-ciphertext value. This pins the derivation to
        // AES-256-GCM (the bug class the spec flags is accidentally using AES-128).
        let uak = derive_unidentified_access_key(&[0u8; 32]).expect("uak");
        // Computed via Python: AES-256-GCM(key=0*32, nonce=0*12).encrypt(0*16) truncated to 16.
        let expected: [u8; 16] = [
            0xce, 0xa7, 0x40, 0x3d, 0x4d, 0x60, 0x6b, 0x6e, 0x07, 0x4e, 0xc5, 0xd3, 0xba, 0xf3,
            0x9d, 0x18,
        ];
        assert_eq!(uak, expected);
    }

    #[test]
    fn attributes_serialize_with_expected_field_names() {
        let attrs = build_account_attributes(
            "ZGV2aWNlLW5hbWU=".to_string(),
            &[0u8; 32],
            42,
            43,
        )
        .expect("build");
        let json = serde_json::to_value(&attrs).expect("serialize");
        // Spot-check the renamed/camelCase fields that Signal's server parses.
        for key in [
            "signalingKey",
            "registrationId",
            "voice",
            "video",
            "fetchesMessages",
            "registrationLock",
            "unidentifiedAccessKey",
            "unrestrictedUnidentifiedAccess",
            "discoverableByPhoneNumber",
            "capabilities",
            "name",
            "pniRegistrationId",
            "recoveryPassword",
        ] {
            assert!(json.get(key).is_some(), "missing {} in AccountAttributes JSON", key);
        }
        let caps = json.get("capabilities").expect("capabilities");
        for key in ["storage", "versionedExpirationTimer", "attachmentBackfill", "ssre2", "spqr"] {
            assert_eq!(caps.get(key), Some(&serde_json::Value::Bool(true)), "capabilities.{} should be true", key);
        }
        assert_eq!(json["signalingKey"], serde_json::Value::Null);
        assert_eq!(json["registrationLock"], serde_json::Value::Null);
        assert_eq!(json["recoveryPassword"], serde_json::Value::Null);
        assert_eq!(json["registrationId"], serde_json::json!(42));
        assert_eq!(json["pniRegistrationId"], serde_json::json!(43));
    }
}
