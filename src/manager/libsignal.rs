// Real libsignal-protocol integration for the device-link provisioning flow.
// Replaces the previous stub implementation.
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use hkdf::Hkdf;
use hmac::Mac as _;
use libsignal_protocol::{IdentityKeyPair as LibIdentityKeyPair, PrivateKey, PublicKey};
use prost::Message as _;
use rand::TryRngCore as _;
use rand::rngs::OsRng;
use sha2::Sha256;
use std::io::{Error, ErrorKind};

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type HmacSha256 = hmac::Hmac<Sha256>;

// ─── Internal protobuf types ──────────────────────────────────────────────────
// Inline prost definitions; no separate .proto build step needed.

// Signal sends all WebSocket messages wrapped in this envelope.
// WebSocketProtos.proto: WebSocketMessage { Type type=1; WebSocketRequestMessage request=2; }
#[derive(prost::Message)]
struct WebSocketRequestMessageProto {
    #[prost(string, optional, tag = "1")]
    verb: Option<String>,
    #[prost(string, optional, tag = "2")]
    path: Option<String>,
    #[prost(bytes = "vec", optional, tag = "3")]
    body: Option<Vec<u8>>,
    #[prost(uint64, optional, tag = "4")]
    id: Option<u64>,
}

#[derive(prost::Message)]
struct WebSocketMessageProto {
    #[prost(int32, optional, tag = "1")]
    r#type: Option<i32>,
    #[prost(message, optional, tag = "2")]
    request: Option<WebSocketRequestMessageProto>,
}

#[derive(prost::Message)]
struct ProvisioningAddressProto {
    // Signal uses the name "address" in newer protos; wire-compatible with
    // the older "uuid" field (same field number 1, same type string).
    #[prost(string, optional, tag = "1")]
    address: Option<String>,
}

#[derive(prost::Message)]
struct ProvisionEnvelopeProto {
    #[prost(bytes = "vec", optional, tag = "1")]
    public_key: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    body: Option<Vec<u8>>,
}

// DeviceName.proto (Signal-Android app/src/main/protowire/DeviceName.proto):
//   optional bytes ephemeralPublic = 1;
//   optional bytes syntheticIv     = 2;
//   optional bytes ciphertext      = 3;
#[derive(prost::Message)]
struct DeviceNameProto {
    #[prost(bytes = "vec", optional, tag = "1")]
    ephemeral_public: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    synthetic_iv: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "3")]
    ciphertext: Option<Vec<u8>>,
}

#[derive(prost::Message)]
struct ProvisionMessageProto {
    #[prost(bytes = "vec", optional, tag = "1")]
    aci_identity_key_public: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    aci_identity_key_private: Option<Vec<u8>>,
    #[prost(string, optional, tag = "3")]
    number: Option<String>,
    #[prost(string, optional, tag = "4")]
    provisioning_code: Option<String>,
    #[prost(bytes = "vec", optional, tag = "6")]
    profile_key: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "7")]
    read_receipts: Option<bool>,
    #[prost(string, optional, tag = "8")]
    aci: Option<String>,
    #[prost(string, optional, tag = "10")]
    pni: Option<String>,
    #[prost(bytes = "vec", optional, tag = "11")]
    pni_identity_key_public: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "12")]
    pni_identity_key_private: Option<Vec<u8>>,
    // tag 13 is reserved (was masterKey, deprecated in favor of accountEntropyPool)
    #[prost(string, optional, tag = "15")]
    account_entropy_pool: Option<String>,
}

// ─── Public types (consumed by manager.rs and account.rs) ────────────────────

pub struct SignalServiceAddress {}
impl SignalServiceAddress {
    // Signal's convention: primary device is 1; linked (secondary) devices
    // are assigned 2+ by the server. Reference: Signal-Android
    // SignalServiceAddress.java L22.
    pub const DEFAULT_DEVICE_ID: u32 = 1;
}

pub struct IdentityKey {
    /// URL-safe no-pad base64 of the serialized key bytes.
    /// Public keys: 33 bytes (0x05 type prefix + 32-byte X25519 key).
    /// Private keys: 32 bytes raw Curve25519 scalar.
    pub key: String,
}

pub struct IdentityKeyPair {
    pub service_id: String,
    pub djb_identity_key: IdentityKey,
    pub djb_private_key: IdentityKey,
}

pub struct ProvisionMessage {
    pub number: String,
    pub aci: IdentityKeyPair,
    pub pni: IdentityKeyPair,
    pub master_key: String,
    pub profile_key: Option<String>,
    /// One-time `provisioningCode` from proto tag 4. Required by the
    /// `PUT /v1/devices/link` REST call (`verificationCode` field).
    pub provisioning_code: Option<String>,
    /// `accountEntropyPool` from proto tag 15. Replaces deprecated tag 13
    /// masterKey; master key derivation from AEP is a separate task.
    pub account_entropy_pool: Option<String>,
}

pub struct ProvisioningUuid {
    pub id: String,
}

pub struct DeviceNameUtil {}
impl DeviceNameUtil {
    /// Encrypt a secondary-device name for Signal's `PUT /v1/devices/link`.
    /// Mirrors Signal-Android's `DeviceNameCipher.encryptDeviceName` so that
    /// the primary device can decrypt and display it:
    ///   1. Generate ephemeral Curve25519 keypair.
    ///   2. master_secret = ECDH(ephemeral_priv, ACI identity_pub).
    ///   3. synthetic_iv  = HMAC-SHA256(HMAC-SHA256(master_secret, "auth"),  plaintext)[..16]
    ///   4. cipher_key    = HMAC-SHA256(HMAC-SHA256(master_secret, "cipher"), synthetic_iv)
    ///   5. ciphertext    = AES-256-CTR(cipher_key, iv=zeros, plaintext)
    ///   6. Output: protobuf { ephemeralPublic, syntheticIv, ciphertext }, base64-encoded.
    pub fn encrypt_device_name(
        device_name: &str,
        aci_identity_priv: IdentityKey,
    ) -> Result<String, Error> {
        use base64::engine::general_purpose::STANDARD;

        // Decode the ACI identity private key (URL-safe base64 no padding, 32-byte scalar).
        let priv_bytes = URL_SAFE_NO_PAD
            .decode(&aci_identity_priv.key)
            .map_err(|e| Error::new(ErrorKind::InvalidData, format!("identity priv b64: {e}")))?;
        let identity_private = PrivateKey::deserialize(&priv_bytes).map_err(|e| {
            Error::new(ErrorKind::InvalidData, format!("identity priv deserialize: {e:?}"))
        })?;
        let identity_public = identity_private.public_key().map_err(|e| {
            Error::other(format!("identity pub derivation: {e:?}"))
        })?;

        // 1. Ephemeral Curve25519 keypair + ECDH.
        let mut rng = OsRng.unwrap_err();
        let ephemeral = libsignal_protocol::KeyPair::generate(&mut rng);
        let master_secret = ephemeral
            .private_key
            .calculate_agreement(&identity_public)
            .map_err(|e| Error::other(format!("ECDH: {e:?}")))?;

        // 2. synthetic_iv_key = HMAC-SHA256(master_secret, "auth")
        let mut mac = HmacSha256::new_from_slice(&master_secret)
            .map_err(|_| Error::new(ErrorKind::InvalidData, "HMAC key init (auth)"))?;
        mac.update(b"auth");
        let synthetic_iv_key = mac.finalize().into_bytes();

        // 3. synthetic_iv = HMAC-SHA256(synthetic_iv_key, plaintext)[..16]
        let mut mac = HmacSha256::new_from_slice(&synthetic_iv_key)
            .map_err(|_| Error::new(ErrorKind::InvalidData, "HMAC key init (synth_iv)"))?;
        mac.update(device_name.as_bytes());
        let synthetic_iv_full = mac.finalize().into_bytes();
        let mut synthetic_iv = [0u8; 16];
        synthetic_iv.copy_from_slice(&synthetic_iv_full[..16]);

        // 4. cipher_key_key = HMAC-SHA256(master_secret, "cipher")
        let mut mac = HmacSha256::new_from_slice(&master_secret)
            .map_err(|_| Error::new(ErrorKind::InvalidData, "HMAC key init (cipher)"))?;
        mac.update(b"cipher");
        let cipher_key_key = mac.finalize().into_bytes();

        // 5. cipher_key = HMAC-SHA256(cipher_key_key, synthetic_iv)
        let mut mac = HmacSha256::new_from_slice(&cipher_key_key)
            .map_err(|_| Error::new(ErrorKind::InvalidData, "HMAC key init (cipher_key)"))?;
        mac.update(&synthetic_iv);
        let cipher_key = mac.finalize().into_bytes();

        // 6. AES-256-CTR with zero IV.
        let ciphertext = aes256_ctr_encrypt(cipher_key.as_slice(), device_name.as_bytes())?;

        // 7. Proto-encode DeviceName.
        let ephemeral_pub = ephemeral.public_key.serialize().to_vec(); // 33 bytes
        let proto = DeviceNameProto {
            ephemeral_public: Some(ephemeral_pub),
            synthetic_iv: Some(synthetic_iv.to_vec()),
            ciphertext: Some(ciphertext),
        };
        let encoded = proto.encode_to_vec();

        Ok(STANDARD.encode(&encoded))
    }
}

fn aes256_ctr_encrypt(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
    if key.len() != 32 {
        return Err(Error::new(ErrorKind::InvalidData, "AES-256 key must be 32 bytes"));
    }
    let cipher = aes::Aes256::new(GenericArray::from_slice(key));
    let mut counter = [0u8; 16];
    let mut out = Vec::with_capacity(plaintext.len());

    for chunk in plaintext.chunks(16) {
        let mut block = GenericArray::clone_from_slice(&counter);
        cipher.encrypt_block(&mut block);
        for (i, &p) in chunk.iter().enumerate() {
            out.push(p ^ block[i]);
        }
        // Big-endian 128-bit counter increment.
        for i in (0..16).rev() {
            counter[i] = counter[i].wrapping_add(1);
            if counter[i] != 0 {
                break;
            }
        }
    }
    Ok(out)
}

pub struct PrimaryProvisioningCipher {}
impl PrimaryProvisioningCipher {
    pub fn new(_unused: Option<String>) -> Self {
        PrimaryProvisioningCipher {}
    }

    /// Decrypt a `ProvisionEnvelope` received over the provisioning WebSocket.
    ///
    /// Protocol (PrimaryProvisioningCipher.encrypt in Signal-Android):
    ///   ephemeral_pub || 0x01 || IV(16) || AES-256-CBC(plaintext) || HMAC-SHA256(32)
    /// where the HMAC covers the version byte + IV + ciphertext, and the
    /// AES key and MAC key are derived via HKDF-SHA256 from the X25519 shared secret.
    pub fn decrypt(
        &self,
        temp_identity: IdentityKeyPair,
        bytes: Vec<u8>,
    ) -> Result<ProvisionMessage, Error> {
        // 1. Decode ProvisionEnvelope protobuf.
        let envelope = ProvisionEnvelopeProto::decode(bytes.as_slice()).map_err(|e| {
            log::error!("ProvisionEnvelope proto decode failed: {e}");
            Error::new(ErrorKind::InvalidData, "failed to decode ProvisionEnvelope")
        })?;

        let ephemeral_pub_bytes = envelope.public_key.ok_or_else(|| {
            log::error!("ProvisionEnvelope missing publicKey");
            Error::new(ErrorKind::InvalidData, "missing publicKey in ProvisionEnvelope")
        })?;

        let body = envelope.body.ok_or_else(|| {
            log::error!("ProvisionEnvelope missing body");
            Error::new(ErrorKind::InvalidData, "missing body in ProvisionEnvelope")
        })?;

        // body: version(1) | IV(16) | ciphertext(≥16) | HMAC-SHA256(32)
        // minimum: 1 + 16 + 16 + 32 = 65 bytes
        if body.len() < 65 {
            log::error!("ProvisionEnvelope body too short: {} bytes", body.len());
            return Err(Error::new(
                ErrorKind::InvalidData,
                "ProvisionEnvelope body too short",
            ));
        }
        if body[0] != 0x01 {
            log::error!("unexpected ProvisionEnvelope version byte: {:#04x}", body[0]);
            return Err(Error::new(
                ErrorKind::InvalidData,
                "unexpected ProvisionEnvelope version",
            ));
        }

        // 2. Reconstruct our temporary private key from stored base64.
        let priv_bytes = URL_SAFE_NO_PAD
            .decode(&temp_identity.djb_private_key.key)
            .map_err(|e| {
                log::error!("base64-decode of temp private key failed: {e}");
                Error::new(ErrorKind::InvalidData, "failed to decode temp private key")
            })?;
        let our_private_key = PrivateKey::deserialize(&priv_bytes).map_err(|e| {
            log::error!("temp private key deserialize failed: {e:?}");
            Error::new(ErrorKind::InvalidData, "invalid temp private key")
        })?;

        // 3. Parse primary's ephemeral public key from the envelope.
        let their_pub_key = PublicKey::deserialize(&ephemeral_pub_bytes).map_err(|e| {
            log::error!("ephemeral public key deserialize failed: {e:?}");
            Error::new(ErrorKind::InvalidData, "invalid ephemeral public key")
        })?;

        // 4. ECDH.
        let shared_secret = our_private_key
            .calculate_agreement(&their_pub_key)
            .map_err(|e| {
                log::error!("ECDH failed: {e:?}");
                Error::new(ErrorKind::InvalidData, "ECDH failed")
            })?;

        // 5. HKDF-SHA256: salt=none, IKM=shared_secret, info="TextSecure Provisioning Message".
        //    Output: 64 bytes → [aes_key (0..32), mac_key (32..64)].
        const HKDF_INFO: &[u8] = b"TextSecure Provisioning Message";
        let hk = Hkdf::<Sha256>::new(None, &shared_secret);
        let mut derived = [0u8; 64];
        hk.expand(HKDF_INFO, &mut derived).map_err(|e| {
            log::error!("HKDF expand failed: {e}");
            Error::new(ErrorKind::InvalidData, "HKDF expand failed")
        })?;
        let (aes_key, mac_key) = derived.split_at(32);

        // 6. Constant-time HMAC-SHA256 verification over version || IV || ciphertext.
        let mac_input = &body[..body.len() - 32];
        let mac_expected = &body[body.len() - 32..];
        let mut mac = HmacSha256::new_from_slice(mac_key).map_err(|_| {
            Error::new(ErrorKind::InvalidData, "HMAC key init failed")
        })?;
        mac.update(mac_input);
        mac.verify_slice(mac_expected).map_err(|_| {
            log::error!("ProvisionEnvelope HMAC-SHA256 verification failed");
            Error::new(ErrorKind::InvalidData, "MAC verification failed")
        })?;

        // 7. AES-256-CBC-PKCS7 decrypt: IV = body[1..17], ciphertext = body[17..len-32].
        let iv = &body[1..17];
        let ciphertext = &body[17..body.len() - 32];
        let mut decryptor =
            Aes256CbcDec::new_from_slices(aes_key, iv).map_err(|_| {
                Error::new(ErrorKind::InvalidData, "AES key/IV length error")
            })?;
        let plaintext = decryptor
            .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
            .map_err(|_| {
                log::error!("AES-256-CBC-PKCS7 decryption failed");
                Error::new(ErrorKind::InvalidData, "AES-CBC-PKCS7 decryption failed")
            })?;

        // 8. Decode ProvisionMessage protobuf.
        let msg = ProvisionMessageProto::decode(plaintext.as_slice()).map_err(|e| {
            log::error!("ProvisionMessage proto decode failed: {e}");
            Error::new(ErrorKind::InvalidData, "failed to decode ProvisionMessage")
        })?;

        let aci_pub = msg.aci_identity_key_public.ok_or_else(|| {
            log::error!("ProvisionMessage missing aciIdentityKeyPublic");
            Error::new(ErrorKind::InvalidData, "missing aciIdentityKeyPublic")
        })?;
        let aci_priv = msg.aci_identity_key_private.ok_or_else(|| {
            log::error!("ProvisionMessage missing aciIdentityKeyPrivate");
            Error::new(ErrorKind::InvalidData, "missing aciIdentityKeyPrivate")
        })?;
        let pni_pub = msg.pni_identity_key_public.unwrap_or_default();
        let pni_priv = msg.pni_identity_key_private.unwrap_or_default();

        log::info!(
            "ProvisionMessage decoded: number={:?}, aci={:?}, provisioning_code_present={}, aep_present={}",
            msg.number,
            msg.aci,
            msg.provisioning_code.is_some(),
            msg.account_entropy_pool.is_some(),
        );

        Ok(ProvisionMessage {
            number: msg.number.unwrap_or_default(),
            aci: IdentityKeyPair {
                service_id: msg.aci.unwrap_or_default(),
                djb_identity_key: IdentityKey {
                    key: URL_SAFE_NO_PAD.encode(&aci_pub),
                },
                djb_private_key: IdentityKey {
                    key: URL_SAFE_NO_PAD.encode(&aci_priv),
                },
            },
            pni: IdentityKeyPair {
                service_id: msg.pni.unwrap_or_default(),
                djb_identity_key: IdentityKey {
                    key: URL_SAFE_NO_PAD.encode(&pni_pub),
                },
                djb_private_key: IdentityKey {
                    key: URL_SAFE_NO_PAD.encode(&pni_priv),
                },
            },
            master_key: String::new(), // field 13 is reserved/deprecated in Provisioning.proto
            profile_key: msg.profile_key.map(|k| URL_SAFE_NO_PAD.encode(&k)),
            provisioning_code: msg.provisioning_code,
            account_entropy_pool: msg.account_entropy_pool,
        })
    }
}

impl ProvisionMessage {
    pub fn decode(
        temp_identity: IdentityKeyPair,
        bytes: Vec<u8>,
    ) -> Result<ProvisionMessage, Error> {
        // Unwrap the WebSocketMessage envelope before decrypting the ProvisionEnvelope body.
        let ws = WebSocketMessageProto::decode(bytes.as_slice()).map_err(|e| {
            log::error!("WebSocketMessage decode failed: {e}");
            Error::new(ErrorKind::InvalidData, "failed to decode WebSocketMessage")
        })?;
        let body = ws
            .request
            .and_then(|r| r.body)
            .ok_or_else(|| {
                log::error!("WebSocketMessage missing request.body for ProvisionEnvelope");
                Error::new(ErrorKind::InvalidData, "missing request.body")
            })?;
        PrimaryProvisioningCipher::new(None).decrypt(temp_identity, body)
    }
}

impl ProvisioningUuid {
    /// Decode a ProvisioningAddress protobuf from a WebSocketMessage frame.
    /// Signal wraps all provisioning messages in WebSocketMessage { type, request: { body } }.
    /// Returns the opaque provisioning address string used as the QR URI uuid parameter.
    pub fn decode(bytes: Vec<u8>) -> Result<ProvisioningUuid, Error> {
        let ws = WebSocketMessageProto::decode(bytes.as_slice()).map_err(|e| {
            log::error!("WebSocketMessage decode failed: {e}");
            Error::new(ErrorKind::InvalidData, "failed to decode WebSocketMessage")
        })?;
        let body = ws
            .request
            .and_then(|r| r.body)
            .ok_or_else(|| {
                log::error!("WebSocketMessage missing request.body");
                Error::new(ErrorKind::InvalidData, "missing request.body")
            })?;
        let proto = ProvisioningAddressProto::decode(body.as_slice()).map_err(|e| {
            log::error!("ProvisioningAddress proto decode failed: {e}");
            Error::new(ErrorKind::InvalidData, "failed to decode ProvisioningAddress")
        })?;
        let address = proto.address.ok_or_else(|| {
            log::error!("ProvisioningAddress missing address field");
            Error::new(ErrorKind::InvalidData, "missing address in ProvisioningAddress")
        })?;
        log::info!("decoded provisioning address: {:?}", address);
        Ok(ProvisioningUuid { id: address })
    }
}

/// Generate a temporary Curve25519 identity keypair for the device-link QR code.
/// Uses OsRng which routes through the Xous TRNG on hardware or the OS on hosted.
/// Returns the keypair with keys encoded as URL-safe no-pad base64 strings.
pub fn generate_identity_key_pair() -> IdentityKeyPair {
    // OsRng in rand 0.9 is a TryRngCore; .unwrap_err() wraps it into a
    // panicking RngCore + CryptoRng, matching the pattern in libsignal's own tests.
    let mut csprng = OsRng.unwrap_err();
    let kp = LibIdentityKeyPair::generate(&mut csprng);
    IdentityKeyPair {
        service_id: String::new(), // populated from ProvisionMessage after scan
        djb_identity_key: IdentityKey {
            // 33 bytes: 0x05 type prefix + 32-byte X25519 public key
            key: URL_SAFE_NO_PAD.encode(kp.identity_key().serialize()),
        },
        djb_private_key: IdentityKey {
            // 32 bytes raw Curve25519 scalar
            key: URL_SAFE_NO_PAD.encode(kp.private_key().serialize()),
        },
    }
}
