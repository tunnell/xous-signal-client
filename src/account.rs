mod credentials;
mod migration;
mod service_environment;
mod storage;

use crate::manager::account_attrs;
use crate::manager::libsignal::{DeviceNameUtil, IdentityKey, ProvisionMessage, SignalServiceAddress};
use crate::manager::prekeys;
use crate::manager::rest;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use credentials::{AccountCredentials, CREDENTIALS_VERSION};
use libsignal_protocol::PrivateKey;
use pddb::Pddb;
pub use service_environment::ServiceEnvironment;
use std::io::{Error, ErrorKind};
use std::str::FromStr;
use storage::{persist_credentials, read_or_migrate, LoadOutcome, PddbCredentialsStore};
use url::{Host, Url};

/// The Account struct is architected as a cache over a single rkyv-
/// serialized credentials blob in the PDDB dict named in `pddb_dict`.
/// The blob lives at the key `credentials::CREDENTIALS_KEY` and holds
/// the full set of persisted account state.
///
/// Replaces the prior 18-key burst-write shape that empirically still
/// triggered a hardware-only first-attempt link crash on Precursor
/// (issue #51) even after the `set_new + batch sync` mitigation
/// (issue #47). The new shape: 1 PDDB allocation + 1 write + 1 sync
/// per persistence operation. See
/// `xous-signal-client-notes/_open-followups/active/blob-credentials/`
/// for the design.
///
/// The struct mirrors the persisted fields in memory (subset — three
/// write-only fields — `account_entropy_pool`, `registration_id`,
/// `pni_registration_id` — live only on disk, in the blob, and are
/// populated at link time but never read back into memory).
#[allow(dead_code)]
pub struct Account {
    pddb: Pddb,
    pddb_dict: String,
    aci_identity_private: Option<String>,
    aci_identity_public: Option<String>,
    aci_service_id: Option<String>,
    device_id: u32,
    encrypted_device_name: Option<String>,
    host: Host,
    is_multi_device: bool,
    number: Option<String>,
    password: Option<String>,
    pin_master_key: Option<String>,
    pni_identity_private: Option<String>,
    pni_identity_public: Option<String>,
    pni_service_id: Option<String>,
    profile_key: Option<String>,
    registered: bool,
    service_environment: ServiceEnvironment,
    storage_key: Option<String>,
    store_last_receive_timestamp: i64,
    store_manifest_version: i64,
    store_manifest: Option<String>,
}

pub const DEFAULT_HOST: &str = "signal.org";

impl Account {
    /// Create a new Account stored in pddb with default values.
    ///
    /// Writes a fresh `AccountCredentials` blob (defaults overlaid
    /// with the supplied `host` and `service_environment`) to the
    /// dict, syncs, and reads it back into the in-memory `Account`.
    ///
    /// # Arguments
    /// * `pddb_dict` — pddb dictionary name to hold the Account.
    /// * `host` — Signal host server (immutable for this account).
    /// * `service_environment` — Signal service environment
    ///   (immutable for this account).
    ///
    /// # Returns
    /// A new `Account` with default values.
    pub fn new(
        pddb_dict: &str,
        host: &Host,
        service_environment: &ServiceEnvironment,
    ) -> Result<Account, Error> {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();

        let creds = AccountCredentials {
            version: CREDENTIALS_VERSION,
            host: host.to_string(),
            service_environment: service_environment.to_string(),
            ..AccountCredentials::default()
        };

        let store = PddbCredentialsStore { pddb: &pddb, dict: pddb_dict };
        persist_credentials(&store, &creds).map_err(|e| {
            log::warn!("Account::new: persist failed: {e:?}");
            Error::new(ErrorKind::Other, "PDDB write failed in Account::new")
        })?;

        Account::read(pddb_dict)
    }

    /// Retrieve an existing Account from the pddb.
    ///
    /// Reads the `AccountCredentials` blob and copies its fields
    /// into the in-memory `Account`. If the blob is absent but
    /// legacy per-key data is present (one-time migration from the
    /// pre-blob shape), `read_or_migrate` migrates it transparently.
    ///
    /// # Arguments
    /// * `pddb_dict` — the pddb dictionary name holding the Account.
    ///
    /// # Returns
    /// An `Account` populated from the persisted state, or
    /// `Err(InvalidData)` if no state exists yet (caller treats as
    /// "no account; offer link/register").
    pub fn read(pddb_dict: &str) -> Result<Account, Error> {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();

        let store = PddbCredentialsStore { pddb: &pddb, dict: pddb_dict };
        let (creds, outcome) = read_or_migrate(&store)?;
        match outcome {
            LoadOutcome::LoadedFromBlob => log::trace!("credentials loaded from blob"),
            LoadOutcome::MigratedFromLegacy => {
                log::info!("credentials migrated from legacy per-key shape to blob")
            }
            LoadOutcome::Fresh => log::info!("no credentials persisted yet (fresh state)"),
        }
        let creds = creds.ok_or_else(|| Error::from(ErrorKind::InvalidData))?;

        let host = parse_host_or_default(&creds.host);
        let service_environment =
            parse_service_environment_or_default(&creds.service_environment);

        Ok(Account {
            pddb,
            pddb_dict: pddb_dict.to_string(),
            aci_identity_private: creds.aci_identity_private,
            aci_identity_public: creds.aci_identity_public,
            aci_service_id: creds.aci_service_id,
            device_id: creds.device_id,
            encrypted_device_name: creds.encrypted_device_name,
            host,
            is_multi_device: creds.is_multi_device,
            number: creds.number,
            password: creds.password,
            pin_master_key: creds.pin_master_key,
            pni_identity_private: creds.pni_identity_private,
            pni_identity_public: creds.pni_identity_public,
            pni_service_id: creds.pni_service_id,
            profile_key: creds.profile_key,
            registered: creds.registered,
            service_environment,
            storage_key: creds.storage_key,
            store_last_receive_timestamp: creds.store_last_receive_timestamp,
            store_manifest_version: creds.store_manifest_version,
            store_manifest: creds.store_manifest,
        })
    }

    /// Delete this Account key/value from the pddb.
    ///
    /// While this Account struct will persist in memory, a subsequent
    /// `Account::read()` will fail.
    pub fn delete(pddb_dict: &str) -> Result<(), Error> {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        pddb.delete_dict(pddb_dict, None)?;
        log::info!("deleted Signal Account from pddb");
        Ok(())
    }

    /// Link to an existing Signal Account as a secondary device.
    ///
    /// Confirm that the state of the Signal Account is OK before
    /// linking.
    /// <https://github.com/AsamK/signal-cli/blob/375bdb79485ec90beb9a154112821a4657740b7a/lib/src/main/java/org/asamk/signal/manager/internal/ProvisioningManagerImpl.java#L200-L239>
    ///
    /// # Arguments
    /// * `device_name` — name to describe this new device.
    /// * `provisioning_msg` — obtained from the Signal server.
    ///
    /// # Returns
    /// `true` on success.
    pub fn link(
        &mut self,
        device_name: &str,
        provisioning_msg: ProvisionMessage,
    ) -> Result<bool, Error> {
        if self.is_primary_device() {
            log::warn!("failed to link device as already registered as primary");
            return Ok(false);
        }

        let verification_code = provisioning_msg.provisioning_code.clone().ok_or_else(|| {
            log::error!("ProvisionMessage missing provisioningCode (tag 4) — cannot link");
            Error::new(ErrorKind::InvalidData, "missing provisioningCode")
        })?;

        let profile_key_b64 = provisioning_msg.profile_key.as_ref().ok_or_else(|| {
            log::error!("ProvisionMessage missing profile_key — cannot derive UAK");
            Error::new(ErrorKind::InvalidData, "missing profile_key")
        })?;
        let profile_key_bytes = URL_SAFE_NO_PAD.decode(profile_key_b64).map_err(|e| {
            log::error!("profile_key base64 decode: {e}");
            Error::new(ErrorKind::InvalidData, "profile_key not valid base64")
        })?;

        let password = account_attrs::generate_link_password()?;
        let registration_id = account_attrs::generate_registration_id()?;
        let pni_registration_id = account_attrs::generate_registration_id()?;

        let aci = provisioning_msg.aci;
        let pni = provisioning_msg.pni;

        let encrypted_name = DeviceNameUtil::encrypt_device_name(
            device_name,
            IdentityKey { key: aci.djb_private_key.key.clone() },
        )?;

        let attrs = account_attrs::build_account_attributes(
            encrypted_name.clone(),
            &profile_key_bytes,
            registration_id,
            pni_registration_id,
        )?;

        let aci_priv = decode_private_key(&aci.djb_private_key.key, "aci")?;
        let pni_priv = decode_private_key(&pni.djb_private_key.key, "pni")?;

        // Diagnostic: check that the public key we derive from each
        // private matches the public sent in the ProvisionMessage. If
        // these diverge, the identity key chain is broken and our
        // signatures will never verify against the server's stored
        // identity key (422 from PreKeySignatureValidator).
        log_identity_chain("aci", &aci_priv, &aci.djb_identity_key.key);
        log_identity_chain("pni", &pni_priv, &pni.djb_identity_key.key);

        let generated = prekeys::generate_prekeys(&aci_priv, &pni_priv)?;

        // Clone attrs so the post-link refresh below has a copy after
        // the link body consumes its move-by-value (issue #16).
        let body = rest::LinkDeviceRequestBody::from_parts(
            verification_code, attrs.clone(), &generated);

        let base_url = self.chat_url()?;
        let response =
            rest::put_devices_link(&base_url, &provisioning_msg.number, &password, &body)?;
        log::info!(
            "device linked: device_id={}, uuid={}, pni={}",
            response.device_id,
            response.uuid,
            response.pni,
        );
        if response.uuid != aci.service_id && !aci.service_id.is_empty() {
            log::warn!(
                "server uuid ({}) differs from ProvisionMessage aci.service_id ({}); using ProvisionMessage value",
                response.uuid,
                aci.service_id,
            );
        }
        if response.pni != pni.service_id && !pni.service_id.is_empty() {
            log::warn!(
                "server pni ({}) differs from ProvisionMessage pni.service_id ({}); using ProvisionMessage value",
                response.pni,
                pni.service_id,
            );
        }

        // Mutate the in-memory cache. The subsequent `persist_credentials`
        // serializes the post-link state to disk in a single PDDB
        // allocation + write + sync — replaces the prior 18-key burst
        // that empirically still triggered hardware first-attempt
        // crashes (issue #51) under FastSpace pressure even after the
        // set_new + batch sync mitigation (#47).
        self.password = Some(password.clone());
        self.device_id = response.device_id;
        self.aci_identity_private = Some(aci.djb_private_key.key.clone());
        self.aci_identity_public = Some(aci.djb_identity_key.key.clone());
        self.aci_service_id = Some(aci.service_id.clone());
        self.pni_identity_private = Some(pni.djb_private_key.key.clone());
        self.pni_identity_public = Some(pni.djb_identity_key.key.clone());
        self.pni_service_id = Some(pni.service_id.clone());
        self.encrypted_device_name = Some(encrypted_name.clone());
        self.is_multi_device = true;
        self.number = Some(provisioning_msg.number.clone());
        self.profile_key = Some(profile_key_b64.clone());
        self.storage_key = None;
        self.store_last_receive_timestamp = 0;
        self.store_manifest_version = -1;
        self.store_manifest = None;
        self.registered = true;

        // Build the post-link credentials snapshot. Note: three fields
        // — `account_entropy_pool`, `registration_id`,
        // `pni_registration_id` — are persisted in the blob but not
        // tracked as in-memory `Account` fields (today they're
        // write-only; the link path is the only writer and no read
        // path consumes them). We pass them through directly into
        // the snapshot so they reach disk.
        let creds = AccountCredentials {
            version: CREDENTIALS_VERSION,
            aci_identity_private: self.aci_identity_private.clone(),
            aci_identity_public: self.aci_identity_public.clone(),
            aci_service_id: self.aci_service_id.clone(),
            pni_identity_private: self.pni_identity_private.clone(),
            pni_identity_public: self.pni_identity_public.clone(),
            pni_service_id: self.pni_service_id.clone(),
            encrypted_device_name: self.encrypted_device_name.clone(),
            number: self.number.clone(),
            password: self.password.clone(),
            pin_master_key: self.pin_master_key.clone(),
            profile_key: self.profile_key.clone(),
            account_entropy_pool: provisioning_msg
                .account_entropy_pool
                .as_deref()
                .map(str::to_string),
            storage_key: self.storage_key.clone(),
            store_manifest: self.store_manifest.clone(),
            device_id: self.device_id,
            registration_id: Some(registration_id),
            pni_registration_id: Some(pni_registration_id),
            is_multi_device: self.is_multi_device,
            registered: self.registered,
            store_last_receive_timestamp: self.store_last_receive_timestamp,
            store_manifest_version: self.store_manifest_version,
            service_environment: self.service_environment.to_string(),
            host: self.host.to_string(),
        };

        let store = PddbCredentialsStore { pddb: &self.pddb, dict: &self.pddb_dict };
        persist_credentials(&store, &creds).map_err(|e| {
            log::warn!("post-link credentials persist failed: {e:?}");
            Error::new(ErrorKind::Other, "PDDB write failed after credentials persist")
        })?;

        // Save prekey private-key records to pddb stores so incoming
        // messages can be decrypted. Must happen AFTER a successful
        // REST link (above).
        prekeys::save_to_pddb(&generated)?;

        // Post-link account-attributes refresh (issue #16). The link
        // body already carries an accountAttributes sub-object on the
        // device record; this PUT updates the canonical per-account
        // record so the server's per-device and per-account views
        // agree. Reference clients (signal-cli, libsignal-service-rs,
        // Signal-Android) all issue this in addition to the link.
        //
        // Non-fatal: the link succeeded above, the message receive
        // path works, and the server-side per-account record can be
        // retried on a future startup. We log the outcome but do not
        // propagate the error.
        let attrs_identifier = format!("{}.{}", aci.service_id, response.device_id);
        match rest::put_accounts_attributes(&base_url, &attrs_identifier, &password, &attrs) {
            Ok(()) => log::info!("post-link account attributes refreshed"),
            Err(e) => log::warn!(
                "post-link account attributes refresh failed (non-fatal): {e}"
            ),
        }

        Ok(true)
    }

    fn chat_url(&self) -> Result<Url, Error> {
        let host_s = self.host.to_string();
        let base = match self.service_environment {
            ServiceEnvironment::Live => format!("https://chat.{host_s}"),
            ServiceEnvironment::Staging => format!("https://chat.staging.{host_s}"),
        };
        Url::parse(&base).map_err(|e| {
            log::error!("invalid chat URL {base}: {e}");
            Error::new(ErrorKind::InvalidData, "invalid chat URL")
        })
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Returns the hostname of Signal's messaging/auth service for
    /// this account, derived from the stored base host and service
    /// environment — matches the host used by `chat_url()` for REST
    /// calls.
    pub fn chat_host(&self) -> String {
        let host_s = self.host.to_string();
        match self.service_environment {
            ServiceEnvironment::Live => format!("chat.{host_s}"),
            ServiceEnvironment::Staging => format!("chat.staging.{host_s}"),
        }
    }

    pub fn is_primary_device(&self) -> bool {
        // Require registered as well: a fresh (unregistered) account
        // with device_id==0 is not a primary device; it is a pre-link
        // placeholder. Without this guard a stuck/corrupt state could
        // misclassify itself as primary once device_id happens to
        // equal DEFAULT_DEVICE_ID.
        self.is_registered() && self.device_id == SignalServiceAddress::DEFAULT_DEVICE_ID
    }

    pub fn is_registered(&self) -> bool {
        // Also treat a partially-linked account as registered: if
        // device_id != 0, aci_service_id and password are present, the
        // link REST call succeeded and all keys are usable — the
        // registered flag just wasn't written yet.
        self.registered
            || (self.device_id != 0
                && self.aci_service_id.is_some()
                && self.password.is_some())
    }

    #[allow(dead_code)]
    pub fn number(&self) -> Option<&str> {
        self.number.as_deref()
    }

    pub fn aci_service_id(&self) -> Option<&str> {
        self.aci_service_id.as_deref()
    }

    pub fn device_id(&self) -> u32 {
        self.device_id
    }

    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    pub fn service_environment(&self) -> &ServiceEnvironment {
        &self.service_environment
    }

    /// Update the account's phone number on disk and in memory.
    ///
    /// Re-serializes the entire credentials blob (full-write
    /// semantics). Acceptable cost — `set_number` is rare (currently
    /// only the dead-code-flagged `account_register` path calls it),
    /// and the blob is small (~1.5 KiB → one PDDB page).
    #[allow(dead_code)]
    pub fn set_number(&mut self, value: &str) -> Result<(), Error> {
        self.number = Some(value.to_string());
        let creds = self.snapshot_credentials();
        let store = PddbCredentialsStore { pddb: &self.pddb, dict: &self.pddb_dict };
        persist_credentials(&store, &creds)
    }

    /// Build an `AccountCredentials` from the in-memory cache.
    ///
    /// Used by `set_number` (and any future post-link mutation
    /// path) before persistence. Three fields not tracked in
    /// `Account` (`account_entropy_pool`, `registration_id`,
    /// `pni_registration_id`) round-trip through the blob via
    /// `Account::read` → blob → `set_number`-driven re-write → blob
    /// is **lossy by design**: this snapshot zeroes them out.
    /// Today no live code path mutates them post-link, so this is a
    /// safe simplification; if a future feature needs them, lift
    /// them into `Account` fields.
    fn snapshot_credentials(&self) -> AccountCredentials {
        AccountCredentials {
            version: CREDENTIALS_VERSION,
            aci_identity_private: self.aci_identity_private.clone(),
            aci_identity_public: self.aci_identity_public.clone(),
            aci_service_id: self.aci_service_id.clone(),
            pni_identity_private: self.pni_identity_private.clone(),
            pni_identity_public: self.pni_identity_public.clone(),
            pni_service_id: self.pni_service_id.clone(),
            encrypted_device_name: self.encrypted_device_name.clone(),
            number: self.number.clone(),
            password: self.password.clone(),
            pin_master_key: self.pin_master_key.clone(),
            profile_key: self.profile_key.clone(),
            account_entropy_pool: None,
            storage_key: self.storage_key.clone(),
            store_manifest: self.store_manifest.clone(),
            device_id: self.device_id,
            registration_id: None,
            pni_registration_id: None,
            is_multi_device: self.is_multi_device,
            registered: self.registered,
            store_last_receive_timestamp: self.store_last_receive_timestamp,
            store_manifest_version: self.store_manifest_version,
            service_environment: self.service_environment.to_string(),
            host: self.host.to_string(),
        }
    }
}

fn log_identity_chain(label: &str, private_key: &PrivateKey, expected_pub_b64url: &str) {
    match private_key.public_key() {
        Ok(derived_pub) => {
            let derived_b64 = URL_SAFE_NO_PAD.encode(derived_pub.serialize());
            if derived_b64 == expected_pub_b64url {
                log::info!(
                    "{label} identity chain OK: derived pub matches ProvisionMessage pub"
                );
            } else {
                log::error!(
                    "{label} identity chain BROKEN: derived_pub={derived_b64}, provision_pub={expected_pub_b64url}"
                );
            }
        }
        Err(e) => log::error!("{label} public_key derivation failed: {e:?}"),
    }
}

/// Parse `creds.host` (a string like `"signal.org"` or
/// `"192.168.100.1"`) into a `url::Host`, defaulting to
/// `DEFAULT_HOST` on parse failure. Mirrors the post-#50 defensive
/// behavior of the prior tuple-match `Account::read`.
fn parse_host_or_default(s: &str) -> Host {
    Host::parse(s).unwrap_or_else(|e| {
        log::warn!(
            "host parse failed (got {:?}: {e}); defaulting to {}",
            s,
            DEFAULT_HOST
        );
        Host::parse(DEFAULT_HOST).expect("DEFAULT_HOST is a valid host literal")
    })
}

/// Parse `creds.service_environment` ("Live" or "Staging") into the
/// typed enum, defaulting to Live on unknown input.
fn parse_service_environment_or_default(s: &str) -> ServiceEnvironment {
    ServiceEnvironment::from_str(s).unwrap_or_else(|_| {
        log::warn!(
            "service_environment parse failed (got {:?}, expected \"Live\" or \"Staging\"); defaulting to Live",
            s
        );
        ServiceEnvironment::Live
    })
}

fn decode_private_key(key_b64url: &str, label: &str) -> Result<PrivateKey, Error> {
    let bytes = URL_SAFE_NO_PAD.decode(key_b64url).map_err(|e| {
        log::error!("{label} private key base64 decode: {e}");
        Error::new(ErrorKind::InvalidData, "identity private key not valid base64")
    })?;
    PrivateKey::deserialize(&bytes).map_err(|e| {
        log::error!("{label} private key deserialize: {e:?}");
        Error::new(ErrorKind::InvalidData, "identity private key invalid")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_host_signal_org() {
        let h = parse_host_or_default("signal.org");
        assert_eq!(h.to_string(), "signal.org");
    }

    #[test]
    fn parse_host_ipv4() {
        let h = parse_host_or_default("192.168.100.1");
        assert_eq!(h.to_string(), "192.168.100.1");
    }

    #[test]
    fn parse_host_garbage_defaults() {
        let h = parse_host_or_default("not a valid host!@#$%");
        assert_eq!(h.to_string(), DEFAULT_HOST);
    }

    #[test]
    fn parse_host_empty_defaults() {
        // url::Host::parse rejects empty strings.
        let h = parse_host_or_default("");
        assert_eq!(h.to_string(), DEFAULT_HOST);
    }

    #[test]
    fn parse_host_default_constant_is_valid() {
        // Sanity: DEFAULT_HOST must itself round-trip through
        // Host::parse, otherwise parse_host_or_default would panic
        // on the fallback path.
        let h = Host::parse(DEFAULT_HOST).expect("DEFAULT_HOST must parse");
        assert_eq!(h.to_string(), DEFAULT_HOST);
    }

    #[test]
    fn parse_service_environment_live() {
        assert!(matches!(
            parse_service_environment_or_default("Live"),
            ServiceEnvironment::Live
        ));
    }

    #[test]
    fn parse_service_environment_staging() {
        assert!(matches!(
            parse_service_environment_or_default("Staging"),
            ServiceEnvironment::Staging
        ));
    }

    #[test]
    fn parse_service_environment_garbage_defaults_to_live() {
        assert!(matches!(
            parse_service_environment_or_default("Production"),
            ServiceEnvironment::Live
        ));
        assert!(matches!(
            parse_service_environment_or_default(""),
            ServiceEnvironment::Live
        ));
        assert!(matches!(
            parse_service_environment_or_default("LIVE"),
            ServiceEnvironment::Live
        ));
    }
}
