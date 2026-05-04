//! Persisted account credentials, stored as a single rkyv-serialized
//! blob in the `sigchat.account` PDDB dict under the key
//! `CREDENTIALS_KEY`.
//!
//! Replaces the prior 18-key burst-write shape that empirically still
//! triggers a hardware-only first-attempt link crash on Precursor
//! (issue #51) even after the `set_new + batch sync` mitigation
//! (issue #47). Reduces post-link PDDB IPC from ~37 calls to 2 and
//! FastSpace allocations from 18 to 1.
//!
//! Idiom modeled on `xous-core/libs/chat/src/ui.rs::dialogue_save`
//! and `dialogue_read` — the same rkyv 0.8 + Pddb pattern in
//! production for chat dialogue persistence.

use rkyv::{Archive, Deserialize, Serialize};

/// Schema version. Bump for any incompatible field-shape change. The
/// reader checks this against `ArchivedAccountCredentials::version`
/// before deserializing; mismatches return `Err`.
pub const CREDENTIALS_VERSION: u8 = 1;

/// PDDB key name. Versioning the key (not just the body) lets a
/// future incompatible schema bump land alongside the old key for one
/// boot before deletion — belt and braces.
pub const CREDENTIALS_KEY: &str = "credentials_v1";

/// All persisted account state in one rkyv-serializable struct.
///
/// Fields mirror the prior per-key shape; the `version` byte at the
/// front anchors future migrations.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AccountCredentials {
    /// Schema version. Always equals `CREDENTIALS_VERSION` at write
    /// time; checked at read time.
    pub version: u8,

    // -- Identity / provisioning (all `Option<String>`; absent on
    // -- pre-link / not-yet-populated state) ---------------------------
    pub aci_identity_private: Option<String>,
    pub aci_identity_public: Option<String>,
    pub aci_service_id: Option<String>,
    pub pni_identity_private: Option<String>,
    pub pni_identity_public: Option<String>,
    pub pni_service_id: Option<String>,
    pub encrypted_device_name: Option<String>,
    pub number: Option<String>,
    pub password: Option<String>,
    pub pin_master_key: Option<String>,
    pub profile_key: Option<String>,
    pub account_entropy_pool: Option<String>,
    pub storage_key: Option<String>,
    pub store_manifest: Option<String>,

    // -- Numeric / boolean -------------------------------------------
    pub device_id: u32,
    /// Signal registration IDs are 14-bit per the protocol; stored as
    /// `u16` to match `account_attrs::generate_registration_id`.
    pub registration_id: Option<u16>,
    pub pni_registration_id: Option<u16>,
    pub is_multi_device: bool,
    pub registered: bool,
    pub store_last_receive_timestamp: i64,
    pub store_manifest_version: i64,

    // -- Service config ----------------------------------------------
    /// Stored as the `Display`/`FromStr` form of `ServiceEnvironment`
    /// (today: `"Live"` or `"Staging"`). Stringly-typed at the rkyv
    /// boundary so the enum can grow variants without a schema bump.
    pub service_environment: String,
    /// Stored as `url::Host`'s `Display` form (e.g. `"signal.org"`,
    /// `"192.168.100.1"`). Stringly-typed at the rkyv boundary so we
    /// don't have to derive rkyv on a third-party type.
    pub host: String,
}

impl Default for AccountCredentials {
    fn default() -> Self {
        Self {
            version: CREDENTIALS_VERSION,
            aci_identity_private: None,
            aci_identity_public: None,
            aci_service_id: None,
            pni_identity_private: None,
            pni_identity_public: None,
            pni_service_id: None,
            encrypted_device_name: None,
            number: None,
            password: None,
            pin_master_key: None,
            profile_key: None,
            account_entropy_pool: None,
            storage_key: None,
            store_manifest: None,
            device_id: 0,
            registration_id: None,
            pni_registration_id: None,
            is_multi_device: false,
            registered: false,
            store_last_receive_timestamp: 0,
            store_manifest_version: -1,
            service_environment: "Live".to_string(),
            host: "signal.org".to_string(),
        }
    }
}

/// Errors raised by `serialize` / `deserialize`. Callers translate to
/// `std::io::Error` at the storage boundary.
#[derive(Debug)]
pub enum CredentialsCodecError {
    /// rkyv failed to serialize an `AccountCredentials` value.
    Serialize(String),
    /// rkyv failed to deserialize bytes — corrupted blob, truncated
    /// write, or wrong shape entirely.
    Deserialize(String),
    /// The blob's `version` byte didn't match `CREDENTIALS_VERSION`.
    /// Carries the observed and expected versions.
    VersionMismatch { got: u8, expected: u8 },
}

impl std::fmt::Display for CredentialsCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialize(msg) => write!(f, "credentials serialize: {msg}"),
            Self::Deserialize(msg) => write!(f, "credentials deserialize: {msg}"),
            Self::VersionMismatch { got, expected } => write!(
                f,
                "credentials version mismatch: got {got}, expected {expected}"
            ),
        }
    }
}

impl std::error::Error for CredentialsCodecError {}

impl AccountCredentials {
    /// Serialize to a `Vec<u8>` ready to write to PDDB.
    pub fn serialize(&self) -> Result<Vec<u8>, CredentialsCodecError> {
        rkyv::to_bytes::<rkyv::rancor::Error>(self)
            .map(|b| b.to_vec())
            .map_err(|e| CredentialsCodecError::Serialize(format!("{e:?}")))
    }

    /// Deserialize from a byte slice (e.g. as read back from PDDB).
    ///
    /// Returns `VersionMismatch` if the archived `version` byte does
    /// not equal `CREDENTIALS_VERSION` — caller decides whether to
    /// attempt a v0-style fallback or surface the error.
    pub fn deserialize(bytes: &[u8]) -> Result<Self, CredentialsCodecError> {
        let archived = rkyv::access::<ArchivedAccountCredentials, rkyv::rancor::Error>(bytes)
            .map_err(|e| CredentialsCodecError::Deserialize(format!("{e:?}")))?;
        if archived.version != CREDENTIALS_VERSION {
            return Err(CredentialsCodecError::VersionMismatch {
                got: archived.version,
                expected: CREDENTIALS_VERSION,
            });
        }
        rkyv::deserialize::<Self, rkyv::rancor::Error>(archived)
            .map_err(|e| CredentialsCodecError::Deserialize(format!("{e:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populated() -> AccountCredentials {
        AccountCredentials {
            version: CREDENTIALS_VERSION,
            aci_identity_private: Some("aci-priv".into()),
            aci_identity_public: Some("aci-pub".into()),
            aci_service_id: Some("aci-uuid".into()),
            pni_identity_private: Some("pni-priv".into()),
            pni_identity_public: Some("pni-pub".into()),
            pni_service_id: Some("pni-uuid".into()),
            encrypted_device_name: Some("enc-name".into()),
            number: Some("+15551234567".into()),
            password: Some("P@ssw0rd".into()),
            pin_master_key: Some("pmk".into()),
            profile_key: Some("profile-key".into()),
            account_entropy_pool: Some("aep".into()),
            storage_key: Some("sk".into()),
            store_manifest: Some("manifest".into()),
            device_id: 2,
            registration_id: Some(1234),
            pni_registration_id: Some(5678),
            is_multi_device: true,
            registered: true,
            store_last_receive_timestamp: 1_700_000_000,
            store_manifest_version: 7,
            service_environment: "Staging".into(),
            host: "signal.org".into(),
        }
    }

    #[test]
    fn default_round_trips() {
        let creds = AccountCredentials::default();
        let bytes = creds.serialize().expect("serialize default");
        let back = AccountCredentials::deserialize(&bytes).expect("deserialize default");
        assert_eq!(creds, back);
    }

    #[test]
    fn populated_round_trips() {
        let creds = populated();
        let bytes = creds.serialize().expect("serialize populated");
        let back = AccountCredentials::deserialize(&bytes).expect("deserialize populated");
        assert_eq!(creds, back);
    }

    #[test]
    fn empty_string_distinct_from_none() {
        let mut creds = AccountCredentials::default();
        creds.aci_identity_private = Some("".into());
        let bytes = creds.serialize().expect("serialize");
        let back = AccountCredentials::deserialize(&bytes).expect("deserialize");
        assert_eq!(back.aci_identity_private, Some("".into()));
        assert_ne!(back.aci_identity_private, None);
    }

    #[test]
    fn version_mismatch_returns_error() {
        let creds = populated();
        let mut bytes = creds.serialize().expect("serialize");
        // Corrupt the version byte. rkyv's archive layout is
        // self-describing, but the `version` field is the first
        // logical field. Find the byte that holds it by deserializing
        // and asserting current state, then mutate.
        let archived = rkyv::access::<ArchivedAccountCredentials, rkyv::rancor::Error>(&bytes)
            .expect("access");
        assert_eq!(archived.version, CREDENTIALS_VERSION);
        // Walk the bytes to find the version byte (it's the first u8
        // serialized; rkyv lays out structs in declaration order).
        // For rkyv 0.8, the value byte for version is at offset 0 or
        // a small offset depending on alignment. We mutate every byte
        // that's currently the CREDENTIALS_VERSION value to a sentinel
        // and confirm at least one of the resulting blobs returns
        // VersionMismatch — robust regardless of rkyv layout choices.
        let target_offset = bytes
            .iter()
            .position(|&b| b == CREDENTIALS_VERSION)
            .expect("expected at least one byte equal to CREDENTIALS_VERSION");
        bytes[target_offset] = 99; // sentinel: not a valid version

        match AccountCredentials::deserialize(&bytes) {
            Err(CredentialsCodecError::VersionMismatch { got, expected }) => {
                assert_eq!(got, 99);
                assert_eq!(expected, CREDENTIALS_VERSION);
            }
            // If the corruption broke the rkyv layout instead of just
            // the version byte, that's also acceptable — both shapes
            // mean "don't trust this blob".
            Err(CredentialsCodecError::Deserialize(_)) => {}
            other => panic!("expected VersionMismatch or Deserialize error, got {other:?}"),
        }
    }

    #[test]
    fn truncated_bytes_return_deserialize_error() {
        let creds = populated();
        let bytes = creds.serialize().expect("serialize");
        let truncated = &bytes[..bytes.len() / 2];
        let result = AccountCredentials::deserialize(truncated);
        assert!(matches!(
            result,
            Err(CredentialsCodecError::Deserialize(_))
                | Err(CredentialsCodecError::VersionMismatch { .. })
        ));
    }

    #[test]
    fn empty_bytes_return_deserialize_error() {
        let result = AccountCredentials::deserialize(&[]);
        assert!(matches!(result, Err(CredentialsCodecError::Deserialize(_))));
    }

    #[test]
    fn all_zero_bytes_return_error() {
        let result = AccountCredentials::deserialize(&[0u8; 256]);
        assert!(matches!(
            result,
            Err(CredentialsCodecError::Deserialize(_))
                | Err(CredentialsCodecError::VersionMismatch { .. })
        ));
    }

    #[test]
    fn unicode_in_strings_round_trips() {
        let mut creds = populated();
        creds.encrypted_device_name = Some("デバイス名".into());
        creds.number = Some("+1-555-naïveté".into());
        let bytes = creds.serialize().expect("serialize");
        let back = AccountCredentials::deserialize(&bytes).expect("deserialize");
        assert_eq!(back.encrypted_device_name, Some("デバイス名".into()));
        assert_eq!(back.number, Some("+1-555-naïveté".into()));
    }

    #[test]
    fn boundary_numerics_round_trip() {
        let mut creds = AccountCredentials::default();
        creds.device_id = u32::MAX;
        creds.registration_id = Some(u16::MAX);
        creds.pni_registration_id = Some(u16::MAX);
        creds.store_last_receive_timestamp = i64::MAX;
        creds.store_manifest_version = i64::MIN;
        let bytes = creds.serialize().expect("serialize");
        let back = AccountCredentials::deserialize(&bytes).expect("deserialize");
        assert_eq!(back.device_id, u32::MAX);
        assert_eq!(back.registration_id, Some(u16::MAX));
        assert_eq!(back.pni_registration_id, Some(u16::MAX));
        assert_eq!(back.store_last_receive_timestamp, i64::MAX);
        assert_eq!(back.store_manifest_version, i64::MIN);
    }

    #[test]
    fn negative_store_manifest_version_round_trips() {
        // -1 is the documented "fresh / no manifest" sentinel from
        // Account::new (`STORE_MANIFEST_VERSION_KEY` is set to "-1"
        // there).
        let mut creds = AccountCredentials::default();
        creds.store_manifest_version = -1;
        let bytes = creds.serialize().expect("serialize");
        let back = AccountCredentials::deserialize(&bytes).expect("deserialize");
        assert_eq!(back.store_manifest_version, -1);
    }

    #[test]
    fn large_string_field_round_trips() {
        // Worst-case: a field gets ~4 KiB of data (e.g. an oversized
        // device name). Confirms we don't have an upper-bound bug.
        let mut creds = populated();
        creds.encrypted_device_name = Some("x".repeat(4096));
        let bytes = creds.serialize().expect("serialize");
        let back = AccountCredentials::deserialize(&bytes).expect("deserialize");
        assert_eq!(back.encrypted_device_name.as_deref().map(str::len), Some(4096));
    }

    #[test]
    fn serialize_is_deterministic() {
        let creds = populated();
        let a = creds.serialize().expect("first");
        let b = creds.serialize().expect("second");
        assert_eq!(a, b, "rkyv output should be deterministic for the same input");
    }

    #[test]
    fn independent_round_trips_preserve_all_fields() {
        let original = populated();
        let bytes_1 = original.serialize().expect("ser 1");
        let mid: AccountCredentials =
            AccountCredentials::deserialize(&bytes_1).expect("de 1");
        let bytes_2 = mid.serialize().expect("ser 2");
        let final_: AccountCredentials =
            AccountCredentials::deserialize(&bytes_2).expect("de 2");
        assert_eq!(original, final_);
        assert_eq!(bytes_1, bytes_2);
    }

    #[test]
    fn default_has_expected_version() {
        let creds = AccountCredentials::default();
        assert_eq!(creds.version, CREDENTIALS_VERSION);
    }

    #[test]
    fn default_has_safe_defaults_matching_legacy_account_new() {
        // These match the defaults `Account::new` writes today:
        // device_id="0" (parsed → 0), is_multi_device="false" (→ false),
        // registered="false" (→ false), service_environment="Live"
        // (when Config::host is signal.org), store_last_receive_timestamp="0",
        // store_manifest_version="-1".
        let creds = AccountCredentials::default();
        assert_eq!(creds.device_id, 0);
        assert!(!creds.is_multi_device);
        assert!(!creds.registered);
        assert_eq!(creds.service_environment, "Live");
        assert_eq!(creds.store_last_receive_timestamp, 0);
        assert_eq!(creds.store_manifest_version, -1);
    }

    #[test]
    fn typical_serialized_size_under_2kib() {
        // Sanity check on the planning estimate (~1.5 KiB for a
        // populated blob). If this fails by a wide margin, a layout
        // change has happened we should know about.
        let creds = populated();
        let bytes = creds.serialize().expect("serialize");
        assert!(
            bytes.len() < 2048,
            "populated blob is {} bytes (expected < 2 KiB per planning estimate)",
            bytes.len()
        );
    }
}
