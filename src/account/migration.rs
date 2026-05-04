//! Pure-rust legacy-key → `AccountCredentials` mapping.
//!
//! Two responsibilities:
//!
//! 1. **`LegacyKeyReader` trait** — abstraction over "give me the
//!    string value at this PDDB key, or `None` if absent." Lets us
//!    unit-test the migration without touching `pddb::Pddb`. The real
//!    implementation in `storage.rs` wraps a `Pddb` reference.
//!
//! 2. **`migrate_from_legacy` function** — reads each of the ~21
//!    legacy keys via the trait, parses defensively (mirroring the
//!    `Account::read` defensive-parsing behavior introduced for
//!    issue #50), and returns a populated `AccountCredentials`.
//!
//! The migration is pure: no I/O effects beyond what `LegacyKeyReader`
//! produces, no allocation beyond the returned struct, no logging
//! side effects (the caller logs).

use super::credentials::{AccountCredentials, CREDENTIALS_VERSION};
use std::str::FromStr;

// Legacy key constants. These mirror the names previously defined as
// private constants in `account.rs` for the per-key shape. We
// duplicate them here (rather than re-export from `account.rs`) so
// the migration module is self-contained and can be unit-tested
// without depending on the full `Account` machinery.
pub(crate) const ACCOUNT_ENTROPY_POOL_KEY: &str = "aep";
pub(crate) const ACI_IDENTITY_PRIVATE_KEY: &str = "aci.identity.private";
pub(crate) const ACI_IDENTITY_PUBLIC_KEY: &str = "aci.identity.public";
pub(crate) const ACI_SERVICE_ID_KEY: &str = "aci.service_id";
pub(crate) const DEVICE_ID_KEY: &str = "device_id";
pub(crate) const ENCRYPTED_DEVICE_NAME_KEY: &str = "encrypted_device_name";
pub(crate) const HOST_KEY: &str = "host";
pub(crate) const IS_MULTI_DEVICE_KEY: &str = "is_multi_device";
pub(crate) const NUMBER_KEY: &str = "number";
pub(crate) const PASSWORD_KEY: &str = "password";
pub(crate) const PIN_MASTER_KEY_KEY: &str = "pin_master_key";
pub(crate) const PNI_IDENTITY_PRIVATE_KEY: &str = "pni.identity.private";
pub(crate) const PNI_IDENTITY_PUBLIC_KEY: &str = "pni.identity.public";
pub(crate) const PNI_REGISTRATION_ID_KEY: &str = "pni.registration_id";
pub(crate) const PNI_SERVICE_ID_KEY: &str = "pni.service_id";
pub(crate) const PROFILE_KEY_KEY: &str = "profile_key";
pub(crate) const REGISTERED_KEY: &str = "registered";
pub(crate) const REGISTRATION_ID_KEY: &str = "registration_id";
pub(crate) const SERVICE_ENVIRONMENT_KEY: &str = "service_environment";
pub(crate) const STORAGE_KEY_KEY: &str = "storage_key";
pub(crate) const STORE_LAST_RECEIVE_TIMESTAMP_KEY: &str = "store_last_receive_timestamp";
pub(crate) const STORE_MANIFEST_VERSION_KEY: &str = "store_manifest_version";
pub(crate) const STORE_MANIFEST_KEY: &str = "store_manifest";

/// All legacy keys that the migration reads. Used by both the
/// migration function and the post-migration cleanup path that
/// deletes them from PDDB.
pub(crate) const LEGACY_KEYS: &[&str] = &[
    ACCOUNT_ENTROPY_POOL_KEY,
    ACI_IDENTITY_PRIVATE_KEY,
    ACI_IDENTITY_PUBLIC_KEY,
    ACI_SERVICE_ID_KEY,
    DEVICE_ID_KEY,
    ENCRYPTED_DEVICE_NAME_KEY,
    HOST_KEY,
    IS_MULTI_DEVICE_KEY,
    NUMBER_KEY,
    PASSWORD_KEY,
    PIN_MASTER_KEY_KEY,
    PNI_IDENTITY_PRIVATE_KEY,
    PNI_IDENTITY_PUBLIC_KEY,
    PNI_REGISTRATION_ID_KEY,
    PNI_SERVICE_ID_KEY,
    PROFILE_KEY_KEY,
    REGISTERED_KEY,
    REGISTRATION_ID_KEY,
    SERVICE_ENVIRONMENT_KEY,
    STORAGE_KEY_KEY,
    STORE_LAST_RECEIVE_TIMESTAMP_KEY,
    STORE_MANIFEST_VERSION_KEY,
    STORE_MANIFEST_KEY,
];

/// Abstraction over "read string value at PDDB key". The real impl
/// wraps `Pddb`; tests use a `HashMap`-backed mock.
pub(crate) trait LegacyKeyReader {
    /// Return the value at `key` if present, `None` otherwise.
    fn read(&self, key: &str) -> Option<String>;
}

/// Default `host` when the legacy `host` key is absent or unparseable.
pub(crate) const DEFAULT_HOST: &str = "signal.org";
/// Default `service_environment` when the legacy key is absent or
/// unparseable.
pub(crate) const DEFAULT_SERVICE_ENVIRONMENT: &str = "Live";

/// True if at least one legacy key has a value. Used to decide
/// whether migration applies (vs. a fresh state where no legacy
/// keys exist).
pub(crate) fn has_any_legacy_value<R: LegacyKeyReader>(reader: &R) -> bool {
    LEGACY_KEYS.iter().any(|k| reader.read(k).is_some())
}

/// Read every legacy key via `reader` and produce an
/// `AccountCredentials`. Defensive: parse failures default the
/// affected field rather than propagating an error, mirroring the
/// `Account::read` defensive-parsing behavior from #50.
///
/// Caller is responsible for logging warnings on field-level parse
/// failures; this function is silent (it has no `log` dependency by
/// design — keeps the unit tests free of log side-effects).
pub(crate) fn migrate_from_legacy<R: LegacyKeyReader>(reader: &R) -> AccountCredentials {
    AccountCredentials {
        version: CREDENTIALS_VERSION,

        aci_identity_private: reader.read(ACI_IDENTITY_PRIVATE_KEY),
        aci_identity_public: reader.read(ACI_IDENTITY_PUBLIC_KEY),
        aci_service_id: reader.read(ACI_SERVICE_ID_KEY),
        pni_identity_private: reader.read(PNI_IDENTITY_PRIVATE_KEY),
        pni_identity_public: reader.read(PNI_IDENTITY_PUBLIC_KEY),
        pni_service_id: reader.read(PNI_SERVICE_ID_KEY),
        encrypted_device_name: reader.read(ENCRYPTED_DEVICE_NAME_KEY),
        number: reader.read(NUMBER_KEY),
        password: reader.read(PASSWORD_KEY),
        pin_master_key: reader.read(PIN_MASTER_KEY_KEY),
        profile_key: reader.read(PROFILE_KEY_KEY),
        account_entropy_pool: reader.read(ACCOUNT_ENTROPY_POOL_KEY),
        storage_key: reader.read(STORAGE_KEY_KEY),
        store_manifest: reader.read(STORE_MANIFEST_KEY),

        device_id: parse_or_default(reader.read(DEVICE_ID_KEY).as_deref(), 0),
        registration_id: reader
            .read(REGISTRATION_ID_KEY)
            .as_deref()
            .and_then(|s| s.parse::<u16>().ok()),
        pni_registration_id: reader
            .read(PNI_REGISTRATION_ID_KEY)
            .as_deref()
            .and_then(|s| s.parse::<u16>().ok()),
        is_multi_device: parse_or_default(
            reader.read(IS_MULTI_DEVICE_KEY).as_deref(),
            false,
        ),
        registered: parse_or_default(reader.read(REGISTERED_KEY).as_deref(), false),
        store_last_receive_timestamp: parse_or_default(
            reader.read(STORE_LAST_RECEIVE_TIMESTAMP_KEY).as_deref(),
            0,
        ),
        store_manifest_version: parse_or_default(
            reader.read(STORE_MANIFEST_VERSION_KEY).as_deref(),
            -1,
        ),

        service_environment: reader
            .read(SERVICE_ENVIRONMENT_KEY)
            .filter(|s| s == "Live" || s == "Staging")
            .unwrap_or_else(|| DEFAULT_SERVICE_ENVIRONMENT.to_string()),

        host: reader
            .read(HOST_KEY)
            .filter(|s| {
                // Accept anything `url::Host::parse` can round-trip.
                // We round-trip rather than just `is_ok` because the
                // serialized form must match what `Host::Display`
                // produces — otherwise the legacy → blob → in-memory
                // path would silently mutate `host`.
                if let Ok(h) = url::Host::parse(s) {
                    h.to_string() == *s
                } else {
                    false
                }
            })
            .unwrap_or_else(|| DEFAULT_HOST.to_string()),
    }
}

/// Parse helper: try `T::from_str` on the given input, fall back to
/// `default` if input is `None` or unparseable. The defensive shape
/// mirrors `Account::read`'s behavior post-#50.
fn parse_or_default<T: FromStr>(s: Option<&str>, default: T) -> T {
    s.and_then(|s| s.parse().ok()).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Test fixture: in-memory legacy key store.
    #[derive(Default, Clone)]
    struct MockReader {
        store: HashMap<String, String>,
    }

    impl MockReader {
        fn new() -> Self {
            Self::default()
        }

        fn put(&mut self, key: &str, value: &str) -> &mut Self {
            self.store.insert(key.to_string(), value.to_string());
            self
        }

        /// Build a fully-populated reader matching what `Account::link`
        /// would have written under the legacy shape.
        fn fully_linked() -> Self {
            let mut r = Self::new();
            r.put(ACI_IDENTITY_PRIVATE_KEY, "aci-priv");
            r.put(ACI_IDENTITY_PUBLIC_KEY, "aci-pub");
            r.put(ACI_SERVICE_ID_KEY, "aci-uuid");
            r.put(PNI_IDENTITY_PRIVATE_KEY, "pni-priv");
            r.put(PNI_IDENTITY_PUBLIC_KEY, "pni-pub");
            r.put(PNI_SERVICE_ID_KEY, "pni-uuid");
            r.put(ENCRYPTED_DEVICE_NAME_KEY, "enc-name");
            r.put(NUMBER_KEY, "+15551234567");
            r.put(PASSWORD_KEY, "P@ssw0rd");
            r.put(PROFILE_KEY_KEY, "profile-key");
            r.put(ACCOUNT_ENTROPY_POOL_KEY, "aep");
            r.put(DEVICE_ID_KEY, "2");
            r.put(REGISTRATION_ID_KEY, "1234");
            r.put(PNI_REGISTRATION_ID_KEY, "5678");
            r.put(IS_MULTI_DEVICE_KEY, "true");
            r.put(REGISTERED_KEY, "true");
            r.put(STORE_LAST_RECEIVE_TIMESTAMP_KEY, "1700000000");
            r.put(STORE_MANIFEST_VERSION_KEY, "7");
            r.put(SERVICE_ENVIRONMENT_KEY, "Live");
            r.put(HOST_KEY, "signal.org");
            // STORAGE_KEY_KEY, STORE_MANIFEST_KEY, PIN_MASTER_KEY_KEY
            // are intentionally absent — `Account::link` never writes
            // them with `Some(...)` (storage_key + store_manifest are
            // explicit `None` calls; pin_master_key is set by a
            // post-link path that doesn't run today).
            r
        }
    }

    impl LegacyKeyReader for MockReader {
        fn read(&self, key: &str) -> Option<String> {
            self.store.get(key).cloned()
        }
    }

    #[test]
    fn empty_reader_produces_default_credentials() {
        let reader = MockReader::new();
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds, AccountCredentials::default());
    }

    #[test]
    fn fully_linked_reader_produces_correct_credentials() {
        let reader = MockReader::fully_linked();
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.version, CREDENTIALS_VERSION);
        assert_eq!(creds.aci_identity_private.as_deref(), Some("aci-priv"));
        assert_eq!(creds.aci_service_id.as_deref(), Some("aci-uuid"));
        assert_eq!(creds.device_id, 2);
        assert_eq!(creds.registration_id, Some(1234));
        assert_eq!(creds.pni_registration_id, Some(5678));
        assert!(creds.is_multi_device);
        assert!(creds.registered);
        assert_eq!(creds.store_last_receive_timestamp, 1_700_000_000);
        assert_eq!(creds.store_manifest_version, 7);
        assert_eq!(creds.service_environment, "Live");
        assert_eq!(creds.host, "signal.org");
        // Optional / absent fields → None
        assert_eq!(creds.storage_key, None);
        assert_eq!(creds.store_manifest, None);
        assert_eq!(creds.pin_master_key, None);
    }

    #[test]
    fn missing_optional_string_fields_become_none() {
        let mut reader = MockReader::fully_linked();
        // Drop several Option-string fields to simulate fresh state.
        reader.store.remove(ACI_IDENTITY_PRIVATE_KEY);
        reader.store.remove(ENCRYPTED_DEVICE_NAME_KEY);
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.aci_identity_private, None);
        assert_eq!(creds.encrypted_device_name, None);
        // Other fields still populated.
        assert_eq!(creds.aci_service_id.as_deref(), Some("aci-uuid"));
    }

    #[test]
    fn unparseable_device_id_defaults_to_zero() {
        let mut reader = MockReader::fully_linked();
        reader.put(DEVICE_ID_KEY, "not-a-number");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.device_id, 0);
    }

    #[test]
    fn unparseable_is_multi_device_defaults_to_false() {
        let mut reader = MockReader::fully_linked();
        reader.put(IS_MULTI_DEVICE_KEY, "yes");
        let creds = migrate_from_legacy(&reader);
        assert!(!creds.is_multi_device);
    }

    #[test]
    fn unparseable_registered_defaults_to_false() {
        let mut reader = MockReader::fully_linked();
        reader.put(REGISTERED_KEY, "garbage");
        let creds = migrate_from_legacy(&reader);
        assert!(!creds.registered);
    }

    #[test]
    fn unparseable_store_manifest_version_defaults_to_minus_one() {
        let mut reader = MockReader::fully_linked();
        reader.put(STORE_MANIFEST_VERSION_KEY, "x");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.store_manifest_version, -1);
    }

    #[test]
    fn unparseable_store_last_receive_timestamp_defaults_to_zero() {
        let mut reader = MockReader::fully_linked();
        reader.put(STORE_LAST_RECEIVE_TIMESTAMP_KEY, "x");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.store_last_receive_timestamp, 0);
    }

    #[test]
    fn unparseable_registration_id_becomes_none_not_default() {
        // registration_id is `Option<u32>`. Unparseable → None.
        let mut reader = MockReader::fully_linked();
        reader.put(REGISTRATION_ID_KEY, "x");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.registration_id, None);
    }

    #[test]
    fn unknown_service_environment_defaults_to_live() {
        let mut reader = MockReader::fully_linked();
        reader.put(SERVICE_ENVIRONMENT_KEY, "Production");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.service_environment, "Live");
    }

    #[test]
    fn missing_service_environment_defaults_to_live() {
        let mut reader = MockReader::fully_linked();
        reader.store.remove(SERVICE_ENVIRONMENT_KEY);
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.service_environment, "Live");
    }

    #[test]
    fn staging_service_environment_round_trips() {
        let mut reader = MockReader::fully_linked();
        reader.put(SERVICE_ENVIRONMENT_KEY, "Staging");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.service_environment, "Staging");
    }

    #[test]
    fn unparseable_host_defaults_to_signal_org() {
        let mut reader = MockReader::fully_linked();
        reader.put(HOST_KEY, "not a valid host!@#$");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.host, "signal.org");
    }

    #[test]
    fn ipv4_host_round_trips() {
        let mut reader = MockReader::fully_linked();
        reader.put(HOST_KEY, "192.168.100.1");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.host, "192.168.100.1");
    }

    #[test]
    fn missing_host_defaults_to_signal_org() {
        let mut reader = MockReader::fully_linked();
        reader.store.remove(HOST_KEY);
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.host, "signal.org");
    }

    #[test]
    fn empty_string_legacy_value_for_optional_field_round_trips() {
        // PDDB legacy never stored "" for an Option<String>, but if
        // some stale state did, we round-trip it as Some("") not as
        // None. Matches the semantics of the underlying PDDB read
        // (presence of the key, not the value, signals Some).
        let mut reader = MockReader::new();
        reader.put(ACI_SERVICE_ID_KEY, "");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.aci_service_id.as_deref(), Some(""));
    }

    #[test]
    fn migrated_blob_round_trips_through_serialization() {
        // End-to-end: legacy → blob → bytes → blob.
        let reader = MockReader::fully_linked();
        let creds = migrate_from_legacy(&reader);
        let bytes = creds.serialize().expect("serialize");
        let back = AccountCredentials::deserialize(&bytes).expect("deserialize");
        assert_eq!(creds, back);
    }

    #[test]
    fn iter_a_2_2_contamination_shape_round_trips() {
        // Reproduces the iter-A.2.2 attempt-1 hardware-observed
        // cross-key contamination shape (per `feat/renode-test-flag`
        // branch's `renode-test-contaminated` feature). Defensive
        // parsing should default the broken numerics; strings pass
        // through unchanged.
        let mut reader = MockReader::new();
        reader.put(DEVICE_ID_KEY, "f"); // single char from password
        reader.put(IS_MULTI_DEVICE_KEY, "z7zWd"); // middle of password
        reader.put(REGISTERED_KEY, "gnal."); // from "signal."
        reader.put(SERVICE_ENVIRONMENT_KEY, "fals"); // truncated false
        reader.put(STORE_MANIFEST_VERSION_KEY, "fa"); // truncated false
        reader.put(HOST_KEY, "192.168.100.1"); // valid IP, not contaminated
        let creds = migrate_from_legacy(&reader);
        // Numeric / boolean / enum: defaulted.
        assert_eq!(creds.device_id, 0);
        assert!(!creds.is_multi_device);
        assert!(!creds.registered);
        assert_eq!(creds.service_environment, "Live");
        assert_eq!(creds.store_manifest_version, -1);
        // Valid host preserved.
        assert_eq!(creds.host, "192.168.100.1");
    }

    #[test]
    fn has_any_legacy_value_false_when_empty() {
        let reader = MockReader::new();
        assert!(!has_any_legacy_value(&reader));
    }

    #[test]
    fn has_any_legacy_value_true_with_one_field() {
        let mut reader = MockReader::new();
        reader.put(DEVICE_ID_KEY, "0");
        assert!(has_any_legacy_value(&reader));
    }

    #[test]
    fn has_any_legacy_value_true_when_fully_linked() {
        let reader = MockReader::fully_linked();
        assert!(has_any_legacy_value(&reader));
    }

    #[test]
    fn legacy_keys_array_has_no_duplicates() {
        let mut sorted: Vec<&&str> = LEGACY_KEYS.iter().collect();
        sorted.sort();
        let unique_count = sorted
            .windows(2)
            .filter(|w| w[0] != w[1])
            .count()
            + 1;
        assert_eq!(unique_count, LEGACY_KEYS.len());
    }

    #[test]
    fn case_sensitive_bool_parse_uppercase_defaults() {
        // Rust's bool::from_str is case-sensitive. "TRUE" / "True"
        // are unparseable and must default to false. This matches
        // the legacy `Account::read` behavior post-#50.
        let mut reader = MockReader::fully_linked();
        reader.put(IS_MULTI_DEVICE_KEY, "TRUE");
        let creds = migrate_from_legacy(&reader);
        assert!(!creds.is_multi_device);

        reader.put(IS_MULTI_DEVICE_KEY, "True");
        let creds = migrate_from_legacy(&reader);
        assert!(!creds.is_multi_device);
    }

    #[test]
    fn whitespace_in_bool_value_defaults() {
        let mut reader = MockReader::fully_linked();
        reader.put(IS_MULTI_DEVICE_KEY, "true ");
        let creds = migrate_from_legacy(&reader);
        assert!(!creds.is_multi_device, "trailing whitespace should not parse as true");
    }

    #[test]
    fn negative_device_id_string_defaults_to_zero() {
        // device_id is u32; "-1" doesn't parse as u32.
        let mut reader = MockReader::fully_linked();
        reader.put(DEVICE_ID_KEY, "-1");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.device_id, 0);
    }

    #[test]
    fn registration_id_above_u16_max_returns_none() {
        // registration_id is Option<u16>; values > u16::MAX (65535)
        // shouldn't ever appear in legacy data (Signal uses 14-bit IDs)
        // but if they did, defensive parsing returns None rather than
        // truncating.
        let mut reader = MockReader::fully_linked();
        reader.put(REGISTRATION_ID_KEY, "100000");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.registration_id, None);
    }

    #[test]
    fn host_with_port_does_not_round_trip() {
        // url::Host::parse on "signal.org:443" succeeds in some
        // libraries but in url 2.x the Host::parse rejects port
        // syntax. The migration filter requires
        // round-trip-equality, so anything that doesn't survive
        // parse + Display falls back to the default.
        let mut reader = MockReader::fully_linked();
        reader.put(HOST_KEY, "signal.org:443");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.host, "signal.org");
    }

    #[test]
    fn ipv6_host_round_trips() {
        // url::Host::parse normalizes IPv6 addresses to a bracketed
        // canonical form. The migration filter accepts only values
        // that round-trip identically, so an unbracketed IPv6 in
        // legacy state would default. A bracketed IPv6 should be
        // preserved.
        let mut reader = MockReader::fully_linked();
        reader.put(HOST_KEY, "[::1]");
        let creds = migrate_from_legacy(&reader);
        assert_eq!(creds.host, "[::1]");
    }

    #[test]
    fn migrate_then_serialize_is_byte_stable() {
        // Determinism: the migration result for a fixed reader,
        // serialized twice, produces the same bytes.
        let reader = MockReader::fully_linked();
        let creds_a = migrate_from_legacy(&reader);
        let creds_b = migrate_from_legacy(&reader);
        assert_eq!(creds_a, creds_b);
        let a = creds_a.serialize().unwrap();
        let b = creds_b.serialize().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn legacy_keys_array_covers_every_account_field() {
        // Sanity: ensure every legacy const we defined is in the
        // LEGACY_KEYS array. If a future contributor adds a
        // legacy key but forgets to register it in LEGACY_KEYS,
        // the migration silently drops that field's value AND
        // the post-migration cleanup leaves it as a stale key.
        let listed: std::collections::HashSet<_> = LEGACY_KEYS.iter().copied().collect();
        for k in &[
            ACCOUNT_ENTROPY_POOL_KEY,
            ACI_IDENTITY_PRIVATE_KEY,
            ACI_IDENTITY_PUBLIC_KEY,
            ACI_SERVICE_ID_KEY,
            DEVICE_ID_KEY,
            ENCRYPTED_DEVICE_NAME_KEY,
            HOST_KEY,
            IS_MULTI_DEVICE_KEY,
            NUMBER_KEY,
            PASSWORD_KEY,
            PIN_MASTER_KEY_KEY,
            PNI_IDENTITY_PRIVATE_KEY,
            PNI_IDENTITY_PUBLIC_KEY,
            PNI_REGISTRATION_ID_KEY,
            PNI_SERVICE_ID_KEY,
            PROFILE_KEY_KEY,
            REGISTERED_KEY,
            REGISTRATION_ID_KEY,
            SERVICE_ENVIRONMENT_KEY,
            STORAGE_KEY_KEY,
            STORE_LAST_RECEIVE_TIMESTAMP_KEY,
            STORE_MANIFEST_VERSION_KEY,
            STORE_MANIFEST_KEY,
        ] {
            assert!(listed.contains(k), "{k} missing from LEGACY_KEYS");
        }
    }
}
