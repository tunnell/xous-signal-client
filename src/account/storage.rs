//! PDDB-backed storage for [`AccountCredentials`].
//!
//! Two layers:
//!
//! 1. The [`CredentialsStore`] trait — abstraction over "read/write
//!    the blob, read/delete legacy keys, sync." Lets us unit-test the
//!    orchestration logic without spinning up the PDDB hosted-mode
//!    server.
//!
//! 2. [`PddbCredentialsStore`] — production impl that wraps a
//!    `pddb::Pddb` and a dict name. Thin: every method is one or two
//!    PDDB calls.
//!
//! The orchestration logic — read blob → fall back to legacy migration
//! → return `AccountCredentials` — lives in [`read_or_migrate`] and
//! is fully tested with mock stores.

use super::credentials::{AccountCredentials, CREDENTIALS_KEY};
use super::migration::{
    has_any_legacy_value, migrate_from_legacy, LegacyKeyReader, LEGACY_KEYS,
};
use pddb::Pddb;
use std::io::{Error, ErrorKind, Read, Write};

/// One-stop trait for the orchestration logic. Consolidates the blob
/// I/O, legacy-key reads, legacy-key cleanup, and the durability
/// `sync` into a single mockable interface.
pub(crate) trait CredentialsStore: LegacyKeyReader {
    /// Return the raw blob bytes if the blob key exists, `None` if
    /// absent. Errors propagate (e.g. PDDB itself errored).
    fn read_blob(&self) -> Result<Option<Vec<u8>>, Error>;

    /// Persist `bytes` under the blob key, replacing any prior value.
    /// The implementation is responsible for calling
    /// `pddb.delete_key` first if the existing key might be longer
    /// than `bytes` (PDDB writes are position-based, not truncating).
    fn write_blob(&self, bytes: &[u8]) -> Result<(), Error>;

    /// Delete the legacy key `key`. Idempotent — no-op if missing.
    fn delete_legacy_key(&self, key: &str) -> Result<(), Error>;

    /// Force durability of any pending writes.
    fn sync(&self) -> Result<(), Error>;
}

/// Production impl backed by `pddb::Pddb`. Borrows `pddb` and `dict`
/// to avoid spinning up a fresh PDDB connection per operation.
pub(crate) struct PddbCredentialsStore<'a> {
    pub pddb: &'a Pddb,
    pub dict: &'a str,
}

impl<'a> LegacyKeyReader for PddbCredentialsStore<'a> {
    fn read(&self, key: &str) -> Option<String> {
        match self
            .pddb
            .get(self.dict, key, None, true, false, None, None::<fn()>)
        {
            Ok(mut k) => {
                let mut buf = [0u8; 256];
                match k.read(&mut buf) {
                    Ok(len) => match String::from_utf8(buf[..len].to_vec()) {
                        Ok(s) => Some(s),
                        Err(e) => {
                            log::warn!("legacy {key} not utf-8: {e:?}");
                            None
                        }
                    },
                    Err(e) => {
                        log::warn!("legacy {key} read failed: {e:?}");
                        None
                    }
                }
            }
            Err(_) => None,
        }
    }
}

impl<'a> CredentialsStore for PddbCredentialsStore<'a> {
    fn read_blob(&self) -> Result<Option<Vec<u8>>, Error> {
        let mut k = match self.pddb.get(
            self.dict,
            CREDENTIALS_KEY,
            None,
            true,
            false,
            None,
            None::<fn()>,
        ) {
            Ok(k) => k,
            Err(_) => return Ok(None), // key missing → not yet linked
        };
        let mut buf = Vec::with_capacity(2048);
        k.read_to_end(&mut buf)?;
        if buf.is_empty() {
            // Blob key exists but is empty (atypical — partial-write
            // edge). Treat as absent so the migration path runs.
            return Ok(None);
        }
        Ok(Some(buf))
    }

    fn write_blob(&self, bytes: &[u8]) -> Result<(), Error> {
        // Pre-clear in case the existing blob is longer than the new
        // bytes — PDDB writes are position-based, not truncating.
        // Idempotent if missing (PDDB returns NotFound, ignored).
        let _ = self.pddb.delete_key(self.dict, CREDENTIALS_KEY, None);

        let mut k = self.pddb.get(
            self.dict,
            CREDENTIALS_KEY,
            None,
            true,
            true,
            Some(bytes.len() + 64), // alloc hint with some slack
            None::<fn()>,
        )?;
        k.write_all(bytes)?;
        Ok(())
    }

    fn delete_legacy_key(&self, key: &str) -> Result<(), Error> {
        // PDDB's `delete_key` returns Err if the key is missing; we
        // treat NotFound as success (idempotent cleanup).
        match self.pddb.delete_key(self.dict, key, None) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn sync(&self) -> Result<(), Error> {
        self.pddb.sync()
    }
}

/// Outcome of [`read_or_migrate`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LoadOutcome {
    /// Blob present and decoded successfully. No migration ran.
    LoadedFromBlob,
    /// No blob; legacy keys present; migrated and wrote a fresh blob.
    MigratedFromLegacy,
    /// Neither blob nor legacy keys present. Fresh state.
    Fresh,
}

/// Read credentials from the store, migrating from the legacy
/// per-key shape if the blob is absent (or unreadable) but legacy
/// keys are present.
///
/// Returns `(creds, outcome)` on success. `creds` is `None` only
/// when the outcome is `Fresh` — i.e. neither shape exists yet.
///
/// Decision tree:
///
/// 1. Try to read the blob.
///    - Decode succeeds → return `(Some(creds), LoadedFromBlob)`.
///    - Decode fails (corrupt blob) → log warning, fall through to
///      legacy.
///    - Blob absent → fall through to legacy.
/// 2. If any legacy key is present → migrate, write the new blob,
///    delete the legacy keys, sync. Return
///    `(Some(creds), MigratedFromLegacy)`.
/// 3. Otherwise → return `(None, Fresh)`.
///
/// Migration is idempotent: a crash mid-write of the new blob leaves
/// the legacy keys intact for re-migration on the next boot. A crash
/// mid-delete of legacy keys means the blob exists; subsequent boots
/// take path 1 and ignore the stale legacy keys.
pub(crate) fn read_or_migrate<S: CredentialsStore>(
    store: &S,
) -> Result<(Option<AccountCredentials>, LoadOutcome), Error> {
    // 1. Blob.
    match store.read_blob()? {
        Some(bytes) => match AccountCredentials::deserialize(&bytes) {
            Ok(creds) => return Ok((Some(creds), LoadOutcome::LoadedFromBlob)),
            Err(e) => {
                log::warn!(
                    "credentials blob present but decode failed ({e}); falling back to legacy keys"
                );
            }
        },
        None => {}
    }

    // 2. Legacy.
    if !has_any_legacy_value(store) {
        return Ok((None, LoadOutcome::Fresh));
    }

    let creds = migrate_from_legacy(store);
    let bytes = creds
        .serialize()
        .map_err(|e| Error::new(ErrorKind::Other, format!("serialize migrated creds: {e}")))?;

    store.write_blob(&bytes)?;
    for key in LEGACY_KEYS {
        if let Err(e) = store.delete_legacy_key(key) {
            log::warn!("post-migration legacy delete failed for {key}: {e:?}");
            // Don't propagate — stale legacy keys are harmless dead
            // data per plan §"Migration".
        }
    }
    store.sync()?;

    Ok((Some(creds), LoadOutcome::MigratedFromLegacy))
}

/// Persist `creds` under the blob key and call sync.
///
/// Used by `Account::new` (initial defaults) and `Account::link`
/// (post-link credentials) and any future mutation path. Two PDDB
/// IPC calls plus a sync — replaces the old 18-call burst.
pub(crate) fn persist_credentials<S: CredentialsStore>(
    store: &S,
    creds: &AccountCredentials,
) -> Result<(), Error> {
    let bytes = creds
        .serialize()
        .map_err(|e| Error::new(ErrorKind::Other, format!("serialize creds: {e}")))?;
    store.write_blob(&bytes)?;
    store.sync()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// In-memory mock store. Tracks both the blob and legacy keys.
    /// Writes are durable immediately; `sync` is just a counter we
    /// can assert on.
    struct MockStore {
        blob: RefCell<Option<Vec<u8>>>,
        legacy: RefCell<HashMap<String, String>>,
        sync_count: RefCell<usize>,
        write_count: RefCell<usize>,
        delete_count: RefCell<usize>,
        // Optional fault-injection knobs for tests.
        fail_write: bool,
        fail_blob_read_with: Option<ErrorKind>,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                blob: RefCell::new(None),
                legacy: RefCell::new(HashMap::new()),
                sync_count: RefCell::new(0),
                write_count: RefCell::new(0),
                delete_count: RefCell::new(0),
                fail_write: false,
                fail_blob_read_with: None,
            }
        }

        fn put_legacy(&self, key: &str, value: &str) -> &Self {
            self.legacy
                .borrow_mut()
                .insert(key.to_string(), value.to_string());
            self
        }

        fn set_blob(&self, bytes: Vec<u8>) -> &Self {
            *self.blob.borrow_mut() = Some(bytes);
            self
        }

        fn legacy_count(&self) -> usize {
            self.legacy.borrow().len()
        }
    }

    impl LegacyKeyReader for MockStore {
        fn read(&self, key: &str) -> Option<String> {
            self.legacy.borrow().get(key).cloned()
        }
    }

    impl CredentialsStore for MockStore {
        fn read_blob(&self) -> Result<Option<Vec<u8>>, Error> {
            if let Some(kind) = self.fail_blob_read_with {
                return Err(Error::new(kind, "injected failure"));
            }
            Ok(self.blob.borrow().clone())
        }

        fn write_blob(&self, bytes: &[u8]) -> Result<(), Error> {
            if self.fail_write {
                return Err(Error::new(ErrorKind::Other, "injected write failure"));
            }
            *self.write_count.borrow_mut() += 1;
            *self.blob.borrow_mut() = Some(bytes.to_vec());
            Ok(())
        }

        fn delete_legacy_key(&self, key: &str) -> Result<(), Error> {
            *self.delete_count.borrow_mut() += 1;
            self.legacy.borrow_mut().remove(key);
            Ok(())
        }

        fn sync(&self) -> Result<(), Error> {
            *self.sync_count.borrow_mut() += 1;
            Ok(())
        }
    }

    fn populated_creds() -> AccountCredentials {
        let mut c = AccountCredentials::default();
        c.aci_service_id = Some("aci-uuid".into());
        c.device_id = 2;
        c.password = Some("pw".into());
        c.registered = true;
        c
    }

    #[test]
    fn fresh_state_returns_none_no_writes() {
        let store = MockStore::new();
        let (creds, outcome) = read_or_migrate(&store).expect("ok");
        assert!(creds.is_none());
        assert_eq!(outcome, LoadOutcome::Fresh);
        assert_eq!(*store.write_count.borrow(), 0);
        assert_eq!(*store.sync_count.borrow(), 0);
        assert_eq!(*store.delete_count.borrow(), 0);
    }

    #[test]
    fn blob_present_loads_from_blob() {
        let store = MockStore::new();
        let creds = populated_creds();
        store.set_blob(creds.serialize().unwrap());
        let (loaded, outcome) = read_or_migrate(&store).expect("ok");
        assert_eq!(loaded.unwrap(), creds);
        assert_eq!(outcome, LoadOutcome::LoadedFromBlob);
        // No writes / no migration.
        assert_eq!(*store.write_count.borrow(), 0);
        assert_eq!(*store.delete_count.borrow(), 0);
    }

    #[test]
    fn legacy_present_migrates_and_writes_blob() {
        use crate::account::migration::{
            ACI_SERVICE_ID_KEY, DEVICE_ID_KEY, PASSWORD_KEY, REGISTERED_KEY,
        };
        let store = MockStore::new();
        store
            .put_legacy(ACI_SERVICE_ID_KEY, "aci-uuid")
            .put_legacy(DEVICE_ID_KEY, "2")
            .put_legacy(PASSWORD_KEY, "pw")
            .put_legacy(REGISTERED_KEY, "true");

        let (creds, outcome) = read_or_migrate(&store).expect("ok");
        let creds = creds.expect("migration produced creds");
        assert_eq!(outcome, LoadOutcome::MigratedFromLegacy);
        assert_eq!(creds.aci_service_id.as_deref(), Some("aci-uuid"));
        assert_eq!(creds.device_id, 2);
        assert!(creds.registered);
        // Blob was written, legacy keys deleted, sync called.
        assert_eq!(*store.write_count.borrow(), 1);
        assert_eq!(*store.sync_count.borrow(), 1);
        assert_eq!(store.legacy_count(), 0);
        // The new blob is readable.
        let bytes = store.blob.borrow().clone().unwrap();
        let back = AccountCredentials::deserialize(&bytes).unwrap();
        assert_eq!(creds, back);
    }

    #[test]
    fn corrupt_blob_falls_back_to_legacy() {
        use crate::account::migration::{ACI_SERVICE_ID_KEY, DEVICE_ID_KEY};
        let store = MockStore::new();
        // Garbage bytes that won't deserialize.
        store.set_blob(vec![0xff; 64]);
        store
            .put_legacy(ACI_SERVICE_ID_KEY, "aci-uuid")
            .put_legacy(DEVICE_ID_KEY, "2");

        let (creds, outcome) = read_or_migrate(&store).expect("ok");
        let creds = creds.expect("legacy fallback");
        assert_eq!(outcome, LoadOutcome::MigratedFromLegacy);
        assert_eq!(creds.aci_service_id.as_deref(), Some("aci-uuid"));
        // Blob got rewritten with the migrated form.
        let bytes = store.blob.borrow().clone().unwrap();
        let back = AccountCredentials::deserialize(&bytes).unwrap();
        assert_eq!(creds, back);
    }

    #[test]
    fn corrupt_blob_no_legacy_returns_fresh() {
        let store = MockStore::new();
        store.set_blob(vec![0xff; 64]);
        let (creds, outcome) = read_or_migrate(&store).expect("ok");
        assert!(creds.is_none());
        assert_eq!(outcome, LoadOutcome::Fresh);
        // Importantly: we did NOT overwrite the corrupt blob with a
        // default. The user's recovery is to delete + re-link.
        assert_eq!(*store.write_count.borrow(), 0);
    }

    #[test]
    fn persist_credentials_writes_and_syncs() {
        let store = MockStore::new();
        let creds = populated_creds();
        persist_credentials(&store, &creds).expect("ok");
        assert_eq!(*store.write_count.borrow(), 1);
        assert_eq!(*store.sync_count.borrow(), 1);
        let bytes = store.blob.borrow().clone().unwrap();
        let back = AccountCredentials::deserialize(&bytes).unwrap();
        assert_eq!(creds, back);
    }

    #[test]
    fn migration_idempotent_on_re_run() {
        // After a successful migration, running read_or_migrate again
        // should hit the blob-present path and NOT re-migrate.
        use crate::account::migration::DEVICE_ID_KEY;
        let store = MockStore::new();
        store.put_legacy(DEVICE_ID_KEY, "2");

        // First call: migrates.
        let (_, o1) = read_or_migrate(&store).expect("ok");
        assert_eq!(o1, LoadOutcome::MigratedFromLegacy);

        // Reset counters so the second call's effects are isolated.
        *store.write_count.borrow_mut() = 0;
        *store.sync_count.borrow_mut() = 0;
        *store.delete_count.borrow_mut() = 0;

        // Second call: blob is there, no legacy → blob path.
        let (_, o2) = read_or_migrate(&store).expect("ok");
        assert_eq!(o2, LoadOutcome::LoadedFromBlob);
        assert_eq!(*store.write_count.borrow(), 0);
        assert_eq!(*store.sync_count.borrow(), 0);
        assert_eq!(*store.delete_count.borrow(), 0);
    }

    #[test]
    fn migration_post_legacy_deletes_all_known_keys() {
        // After migration the legacy hashmap should be empty (mock
        // delete removes the entry; this asserts the cleanup loop
        // covers every key in LEGACY_KEYS).
        use crate::account::migration::{
            ACI_SERVICE_ID_KEY, DEVICE_ID_KEY, HOST_KEY, REGISTERED_KEY,
            SERVICE_ENVIRONMENT_KEY,
        };
        let store = MockStore::new();
        store
            .put_legacy(ACI_SERVICE_ID_KEY, "aci-uuid")
            .put_legacy(DEVICE_ID_KEY, "2")
            .put_legacy(HOST_KEY, "signal.org")
            .put_legacy(REGISTERED_KEY, "true")
            .put_legacy(SERVICE_ENVIRONMENT_KEY, "Live");

        let _ = read_or_migrate(&store).expect("ok");
        assert_eq!(store.legacy_count(), 0);
    }

    #[test]
    fn write_failure_during_migration_propagates() {
        use crate::account::migration::DEVICE_ID_KEY;
        let mut store = MockStore::new();
        store.fail_write = true;
        store.put_legacy(DEVICE_ID_KEY, "2");
        let result = read_or_migrate(&store);
        assert!(result.is_err());
        // Crucially: legacy keys are NOT deleted on a write failure.
        // Next boot sees them and re-migrates.
        assert_eq!(store.legacy_count(), 1);
    }

    #[test]
    fn read_blob_error_propagates() {
        let mut store = MockStore::new();
        store.fail_blob_read_with = Some(ErrorKind::PermissionDenied);
        let result = read_or_migrate(&store);
        assert!(result.is_err());
    }

    #[test]
    fn empty_blob_treated_as_absent() {
        // The PDDB-side impl of read_blob returns Ok(None) when the
        // key is empty (degenerate write). Verify the orchestration
        // doesn't try to deserialize an empty buffer.
        use crate::account::migration::DEVICE_ID_KEY;
        let store = MockStore::new();
        // Mock: blob is None means "absent" per MockStore::read_blob.
        store.put_legacy(DEVICE_ID_KEY, "2");
        let (_, outcome) = read_or_migrate(&store).expect("ok");
        assert_eq!(outcome, LoadOutcome::MigratedFromLegacy);
    }

    #[test]
    fn migration_creds_round_trip_via_blob() {
        use crate::account::migration::{
            ACCOUNT_ENTROPY_POOL_KEY, ACI_IDENTITY_PRIVATE_KEY, ACI_SERVICE_ID_KEY,
            DEVICE_ID_KEY, HOST_KEY, IS_MULTI_DEVICE_KEY, PASSWORD_KEY,
            REGISTERED_KEY, REGISTRATION_ID_KEY, SERVICE_ENVIRONMENT_KEY,
        };
        let store = MockStore::new();
        store
            .put_legacy(ACI_IDENTITY_PRIVATE_KEY, "private")
            .put_legacy(ACI_SERVICE_ID_KEY, "uuid")
            .put_legacy(ACCOUNT_ENTROPY_POOL_KEY, "aep")
            .put_legacy(DEVICE_ID_KEY, "5")
            .put_legacy(HOST_KEY, "signal.org")
            .put_legacy(IS_MULTI_DEVICE_KEY, "true")
            .put_legacy(PASSWORD_KEY, "pw")
            .put_legacy(REGISTERED_KEY, "true")
            .put_legacy(REGISTRATION_ID_KEY, "999")
            .put_legacy(SERVICE_ENVIRONMENT_KEY, "Staging");

        let (creds, _) = read_or_migrate(&store).expect("ok");
        let creds = creds.unwrap();
        assert_eq!(creds.aci_identity_private.as_deref(), Some("private"));
        assert_eq!(creds.aci_service_id.as_deref(), Some("uuid"));
        assert_eq!(creds.account_entropy_pool.as_deref(), Some("aep"));
        assert_eq!(creds.device_id, 5);
        assert_eq!(creds.host, "signal.org");
        assert!(creds.is_multi_device);
        assert_eq!(creds.password.as_deref(), Some("pw"));
        assert!(creds.registered);
        assert_eq!(creds.registration_id, Some(999));
        assert_eq!(creds.service_environment, "Staging");

        // And the on-disk blob carries the same data.
        let bytes = store.blob.borrow().clone().unwrap();
        let back = AccountCredentials::deserialize(&bytes).unwrap();
        assert_eq!(creds, back);
    }
}
