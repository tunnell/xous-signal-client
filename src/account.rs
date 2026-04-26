mod service_environment;

use crate::manager::account_attrs;
use crate::manager::libsignal::{DeviceNameUtil, IdentityKey, ProvisionMessage, SignalServiceAddress};
use crate::manager::prekeys;
use crate::manager::rest;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use libsignal_protocol::PrivateKey;
use pddb::Pddb;
pub use service_environment::ServiceEnvironment;
use std::io::{Error, ErrorKind, Read, Write};
use std::str::FromStr;
use url::{Host, Url};

/// The Account struct is architected as a cache over a pddb dictionary.
///
/// * Creating a new Account inherently involves writing to pddb
/// * Each field has a 1:1 relationship with a pddb.key.
/// * Field values are able to be set individually
///
/// Steps to ensure consistency:
/// * the default values in a new Account are first written to pddb, and then read back into the struct.
/// * all fields must be successfully read from pddb or read fails with Error
/// * setting a value requires a successful writes to pddb before updating the field
///

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

const ACCOUNT_ENTROPY_POOL_KEY: &str = "aep";
const ACI_IDENTITY_PRIVATE_KEY: &str = "aci.identity.private";
const ACI_IDENTITY_PUBLIC_KEY: &str = "aci.identity.public";
const ACI_SERVICE_ID_KEY: &str = "aci.service_id";
const DEVICE_ID_KEY: &str = "device_id";
const ENCRYPTED_DEVICE_NAME_KEY: &str = "encrypted_device_name";
const HOST_KEY: &str = "host";
const IS_MULTI_DEVICE_KEY: &str = "is_multi_device";
const NUMBER_KEY: &str = "number";
const PASSWORD_KEY: &str = "password";
const PIN_MASTER_KEY_KEY: &str = "pin_master_key";
const PNI_IDENTITY_PRIVATE_KEY: &str = "pni.identity.private";
const PNI_IDENTITY_PUBLIC_KEY: &str = "pni.identity.public";
const PNI_REGISTRATION_ID_KEY: &str = "pni.registration_id";
const PNI_SERVICE_ID_KEY: &str = "pni.service_id";
const PROFILE_KEY_KEY: &str = "profile_key";
const REGISTERED_KEY: &str = "registered";
const REGISTRATION_ID_KEY: &str = "registration_id";
const SERVICE_ENVIRONMENT_KEY: &str = "service_environment";
const STORAGE_KEY_KEY: &str = "storage_key";
const STORE_LAST_RECEIVE_TIMESTAMP_KEY: &str = "store_last_receive_timestamp";
const STORE_MANIFEST_VERSION_KEY: &str = "store_manifest_version";
const STORE_MANIFEST_KEY: &str = "store_manifest";

impl Account {
    /// Create a new Account stored in pddb with default values
    ///
    /// This function saves default values for each field in the pddb
    /// and then calls read() to load the values into the Account struct
    ///
    /// # Arguments
    /// * `pddb_dict` - pddb dictionary name to hold the Account
    /// * `host` - Signal host server (immutable)
    /// * `service_environment` - Signal service-environment (immutable)
    ///
    /// # Returns
    ///
    /// a new Account with default values
    ///
    pub fn new(
        pddb_dict: &str,
        host: &Host,
        service_environment: &ServiceEnvironment,
    ) -> Result<Account, Error> {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        set(&pddb, pddb_dict, ACI_IDENTITY_PRIVATE_KEY, None)?;
        set(&pddb, pddb_dict, ACI_IDENTITY_PUBLIC_KEY, None)?;
        set(&pddb, pddb_dict, ACI_SERVICE_ID_KEY, None)?;
        set(&pddb, pddb_dict, DEVICE_ID_KEY, Some("0"))?;
        set(&pddb, pddb_dict, ENCRYPTED_DEVICE_NAME_KEY, None)?;
        set(&pddb, pddb_dict, HOST_KEY, Some(&host.to_string()))?;
        set(
            &pddb,
            pddb_dict,
            IS_MULTI_DEVICE_KEY,
            Some(&false.to_string()),
        )?;
        set(&pddb, pddb_dict, NUMBER_KEY, None)?;
        set(&pddb, pddb_dict, PASSWORD_KEY, None)?;
        set(&pddb, pddb_dict, PIN_MASTER_KEY_KEY, None)?;
        set(&pddb, pddb_dict, PNI_IDENTITY_PRIVATE_KEY, None)?;
        set(&pddb, pddb_dict, PNI_IDENTITY_PUBLIC_KEY, None)?;
        set(&pddb, pddb_dict, PNI_SERVICE_ID_KEY, None)?;
        set(&pddb, pddb_dict, PROFILE_KEY_KEY, None)?;
        set(&pddb, pddb_dict, REGISTERED_KEY, Some(&false.to_string()))?;
        set(
            &pddb,
            pddb_dict,
            SERVICE_ENVIRONMENT_KEY,
            Some(&service_environment.to_string()),
        )?;
        set(&pddb, pddb_dict, STORAGE_KEY_KEY, None)?;
        set(
            &pddb,
            pddb_dict,
            STORE_LAST_RECEIVE_TIMESTAMP_KEY,
            Some("0"),
        )?;
        set(&pddb, pddb_dict, STORE_MANIFEST_VERSION_KEY, Some("-1"))?;
        set(&pddb, pddb_dict, STORE_MANIFEST_KEY, None)?;
        Account::read(pddb_dict)
    }

    // retrieves an existing Account from the pddb
    //
    // # Arguments
    // * `pddb_dict` - the pddb dictionary name holding the Account
    //
    // # Returns
    //
    // a Account with values read from pddb_dict
    //
    pub fn read(pddb_dict: &str) -> Result<Account, Error> {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        match (
            get(&pddb, pddb_dict, ACI_IDENTITY_PRIVATE_KEY),
            get(&pddb, pddb_dict, ACI_IDENTITY_PUBLIC_KEY),
            get(&pddb, pddb_dict, ACI_SERVICE_ID_KEY),
            get(&pddb, pddb_dict, DEVICE_ID_KEY),
            get(&pddb, pddb_dict, ENCRYPTED_DEVICE_NAME_KEY),
            get(&pddb, pddb_dict, HOST_KEY),
            get(&pddb, pddb_dict, IS_MULTI_DEVICE_KEY),
            get(&pddb, pddb_dict, NUMBER_KEY),
            get(&pddb, pddb_dict, PASSWORD_KEY),
            get(&pddb, pddb_dict, PIN_MASTER_KEY_KEY),
            get(&pddb, pddb_dict, PNI_IDENTITY_PRIVATE_KEY),
            get(&pddb, pddb_dict, PNI_IDENTITY_PUBLIC_KEY),
            get(&pddb, pddb_dict, PNI_SERVICE_ID_KEY),
            get(&pddb, pddb_dict, PROFILE_KEY_KEY),
            get(&pddb, pddb_dict, REGISTERED_KEY),
            get(&pddb, pddb_dict, SERVICE_ENVIRONMENT_KEY),
            get(&pddb, pddb_dict, STORAGE_KEY_KEY),
            get(&pddb, pddb_dict, STORE_LAST_RECEIVE_TIMESTAMP_KEY),
            get(&pddb, pddb_dict, STORE_MANIFEST_VERSION_KEY),
            get(&pddb, pddb_dict, STORE_MANIFEST_KEY),
        ) {
            (
                Ok(aci_identity_private),
                Ok(aci_identity_public),
                Ok(aci_service_id),
                Ok(Some(device_id)),
                Ok(encrypted_device_name),
                Ok(Some(host)),
                Ok(Some(is_multi_device)),
                Ok(number),
                Ok(password),
                Ok(pin_master_key),
                Ok(pni_identity_private),
                Ok(pni_identity_public),
                Ok(pni_service_id),
                Ok(profile_key),
                Ok(Some(registered)),
                Ok(Some(service_environment)),
                Ok(storage_key),
                Ok(store_last_receive_timestamp_opt),
                Ok(Some(store_manifest_version)),
                Ok(store_manifest),
            ) => Ok(Account {
                pddb: pddb,
                pddb_dict: pddb_dict.to_string(),
                aci_identity_private: aci_identity_private,
                aci_identity_public: aci_identity_public,
                aci_service_id: aci_service_id,
                device_id: device_id.parse().unwrap(),
                encrypted_device_name: encrypted_device_name,
                host: Host::parse(&host).unwrap(),
                is_multi_device: is_multi_device.parse().unwrap(),
                number: number,
                password: password,
                pin_master_key: pin_master_key,
                pni_identity_private: pni_identity_private,
                pni_identity_public: pni_identity_public,
                pni_service_id: pni_service_id,
                profile_key: profile_key,
                registered: registered.parse().unwrap(),
                service_environment: ServiceEnvironment::from_str(&service_environment).unwrap(),
                storage_key: storage_key,
                store_last_receive_timestamp: store_last_receive_timestamp_opt
                    .as_deref()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
                store_manifest_version: store_manifest_version.parse().unwrap(),
                store_manifest: store_manifest,
            }),
            (Err(e), _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _) => Err(e),
            (_, Err(e), _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _) => Err(e),
            (_, _, Err(e), _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _) => Err(e),
            (_, _, _, Err(e), _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _) => Err(e),
            (_, _, _, _, Err(e), _, _, _, _, _, _, _, _, _, _, _, _, _, _, _) => Err(e),
            (_, _, _, _, _, Err(e), _, _, _, _, _, _, _, _, _, _, _, _, _, _) => Err(e),
            (_, _, _, _, _, _, Err(e), _, _, _, _, _, _, _, _, _, _, _, _, _) => Err(e),
            (_, _, _, _, _, _, _, Err(e), _, _, _, _, _, _, _, _, _, _, _, _) => Err(e),
            (_, _, _, _, _, _, _, _, Err(e), _, _, _, _, _, _, _, _, _, _, _) => Err(e),
            (_, _, _, _, _, _, _, _, _, Err(e), _, _, _, _, _, _, _, _, _, _) => Err(e),
            (_, _, _, _, _, _, _, _, _, _, Err(e), _, _, _, _, _, _, _, _, _) => Err(e),
            (_, _, _, _, _, _, _, _, _, _, _, Err(e), _, _, _, _, _, _, _, _) => Err(e),
            (_, _, _, _, _, _, _, _, _, _, _, _, Err(e), _, _, _, _, _, _, _) => Err(e),
            (_, _, _, _, _, _, _, _, _, _, _, _, _, Err(e), _, _, _, _, _, _) => Err(e),
            (_, _, _, _, _, _, _, _, _, _, _, _, _, _, Err(e), _, _, _, _, _) => Err(e),
            (_, _, _, _, _, _, _, _, _, _, _, _, _, _, _, Err(e), _, _, _, _) => Err(e),
            (_, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, Err(e), _, _, _) => Err(e),
            (_, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, Err(e), _, _) => Err(e),
            (_, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, Err(e), _) => Err(e),
            (_, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, Err(e)) => Err(e),
            (_, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _) => {
                Err(Error::from(ErrorKind::InvalidData))
            }
        }
    }

    /// Delete this Account key/value from the pddb
    ///
    /// While this Account struct will persist in memory, a subsequent Account.read() will fail
    ///
    pub fn delete(pddb_dict: &str) -> Result<(), Error> {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        pddb.delete_dict(pddb_dict, None)?;
        log::info!("deleted Signal Account from pddb");
        Ok(())
    }

    /// link to an existing Signal Account as a secondary device
    ///
    /// Confirm that the state of the Signal Account is OK before linking
    /// https://github.com/AsamK/signal-cli/blob/375bdb79485ec90beb9a154112821a4657740b7a/lib/src/main/java/org/asamk/signal/manager/internal/ProvisioningManagerImpl.java#L200-L239
    ///
    /// # Arguments
    ///
    /// * `device_name` - name to describe this new device
    /// * `provisioning_msg` - obtained from the Signal server
    ///
    /// # Returns
    ///
    /// true on success
    ///
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

        // Diagnostic: check that the public key we derive from each private
        // matches the public sent in the ProvisionMessage. If these diverge,
        // the identity key chain is broken and our signatures will never
        // verify against the server's stored identity key (422 from
        // PreKeySignatureValidator).
        log_identity_chain("aci", &aci_priv, &aci.djb_identity_key.key);
        log_identity_chain("pni", &pni_priv, &pni.djb_identity_key.key);

        let generated = prekeys::generate_prekeys(&aci_priv, &pni_priv)?;

        let body = rest::LinkDeviceRequestBody::from_parts(verification_code, attrs, &generated);

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

        self.set(PASSWORD_KEY, Some(&password))?;
        self.set(DEVICE_ID_KEY, Some(&response.device_id.to_string()))?;
        self.set(ACI_IDENTITY_PRIVATE_KEY, Some(&aci.djb_private_key.key))?;
        self.set(ACI_IDENTITY_PUBLIC_KEY, Some(&aci.djb_identity_key.key))?;
        self.set(ACI_SERVICE_ID_KEY, Some(&aci.service_id))?;
        self.set(PNI_IDENTITY_PRIVATE_KEY, Some(&pni.djb_private_key.key))?;
        self.set(PNI_IDENTITY_PUBLIC_KEY, Some(&pni.djb_identity_key.key))?;
        self.set(PNI_SERVICE_ID_KEY, Some(&pni.service_id))?;
        self.set(ENCRYPTED_DEVICE_NAME_KEY, Some(&encrypted_name))?;
        self.set(IS_MULTI_DEVICE_KEY, Some(&true.to_string()))?;
        self.set(NUMBER_KEY, Some(&provisioning_msg.number))?;
        self.set(PROFILE_KEY_KEY, Some(profile_key_b64))?;
        if let Some(aep) = provisioning_msg.account_entropy_pool.as_deref() {
            self.set(ACCOUNT_ENTROPY_POOL_KEY, Some(aep))?;
        }
        self.set(REGISTRATION_ID_KEY, Some(&registration_id.to_string()))?;
        self.set(
            PNI_REGISTRATION_ID_KEY,
            Some(&pni_registration_id.to_string()),
        )?;
        self.set(STORAGE_KEY_KEY, None)?;
        self.set(STORE_LAST_RECEIVE_TIMESTAMP_KEY, Some("0"))?;
        self.set(STORE_MANIFEST_VERSION_KEY, Some("-1"))?;
        self.set(STORE_MANIFEST_KEY, None)?;

        self.set(REGISTERED_KEY, Some(&true.to_string()))?;

        // Save prekey private-key records to pddb stores so incoming messages
        // can be decrypted. Must happen AFTER a successful REST link (above).
        prekeys::save_to_pddb(&generated)?;

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

    /// Returns the hostname of Signal's messaging/auth service for this account,
    /// derived from the stored base host and service environment — matches the
    /// host used by `chat_url()` for REST calls.
    pub fn chat_host(&self) -> String {
        let host_s = self.host.to_string();
        match self.service_environment {
            ServiceEnvironment::Live => format!("chat.{host_s}"),
            ServiceEnvironment::Staging => format!("chat.staging.{host_s}"),
        }
    }

    pub fn is_primary_device(&self) -> bool {
        // Require registered as well: a fresh (unregistered) account with
        // device_id==0 is not a primary device; it is a pre-link placeholder.
        // Without this guard a stuck/corrupt state could misclassify itself
        // as primary once device_id happens to equal DEFAULT_DEVICE_ID.
        self.is_registered() && self.device_id == SignalServiceAddress::DEFAULT_DEVICE_ID
    }

    pub fn is_registered(&self) -> bool {
        // Also treat a partially-linked account as registered: if device_id != 0,
        // aci_service_id and password are present, the link REST call succeeded and
        // all keys are usable — the registered flag just wasn't written yet.
        self.registered
            || (self.device_id != 0
                && self.aci_service_id.is_some()
                && self.password.is_some())
    }

    #[allow(dead_code)]
    pub fn number(&self) -> Option<&str> {
        match &self.number {
            Some(num) => Some(&num),
            None => None,
        }
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

    #[allow(dead_code)]
    pub fn set_number(&mut self, value: &str) -> Result<(), Error> {
        match self.set(NUMBER_KEY, Some(value)) {
            Ok(_) => self.number = Some(value.to_string()),
            Err(e) => log::warn!("failed to set signal account number: {e}"),
        }
        Ok(())
    }

    #[allow(dead_code)]
    fn get(&self, key: &str) -> Result<Option<String>, Error> {
        get(&self.pddb, &self.pddb_dict, key)
    }

    // Sets the value of a pddb_key / field in the Account
    //
    // To guarantee consistency, the value is saved to the pddb and,
    // on success, set to the corresponding field in the Account struct.
    //
    // # Arguments
    // * `key` - the pddb_key corresponding to the Account field
    // * `value` - the value to save into the Account field (and pddb)
    //
    // # Returns
    //
    // Ok()
    //
    fn set(&mut self, key: &str, value: Option<&str>) -> Result<(), Error> {
        let owned_value = value.map(str::to_string);
        match set(&self.pddb, &self.pddb_dict, key, value) {
            Ok(()) => match key {
                ACI_IDENTITY_PRIVATE_KEY => Ok(self.aci_identity_private = owned_value),
                ACI_IDENTITY_PUBLIC_KEY => Ok(self.aci_identity_public = owned_value),
                ACI_SERVICE_ID_KEY => Ok(self.aci_service_id = owned_value),
                DEVICE_ID_KEY => Ok(self.device_id = owned_value.unwrap().parse().unwrap()),
                ENCRYPTED_DEVICE_NAME_KEY => Ok(self.encrypted_device_name = owned_value),
                IS_MULTI_DEVICE_KEY => {
                    Ok(self.is_multi_device = owned_value.unwrap().parse().unwrap())
                }
                NUMBER_KEY => Ok(self.number = owned_value),
                PASSWORD_KEY => Ok(self.password = owned_value),
                PIN_MASTER_KEY_KEY => Ok(self.pin_master_key = owned_value),
                PNI_IDENTITY_PRIVATE_KEY => Ok(self.pni_identity_private = owned_value),
                PNI_IDENTITY_PUBLIC_KEY => Ok(self.pni_identity_public = owned_value),
                PNI_SERVICE_ID_KEY => Ok(self.pni_service_id = owned_value),
                PROFILE_KEY_KEY => Ok(self.profile_key = owned_value),
                REGISTERED_KEY => Ok(self.registered = owned_value.unwrap().parse().unwrap()),
                SERVICE_ENVIRONMENT_KEY => Ok(self.service_environment =
                    ServiceEnvironment::from_str(&value.unwrap()).unwrap()),
                STORAGE_KEY_KEY => Ok(self.storage_key = owned_value),
                ACCOUNT_ENTROPY_POOL_KEY
                | REGISTRATION_ID_KEY
                | PNI_REGISTRATION_ID_KEY
                | STORE_LAST_RECEIVE_TIMESTAMP_KEY
                | STORE_MANIFEST_VERSION_KEY
                | STORE_MANIFEST_KEY => Ok(()),
                _ => {
                    log::warn!("invalid key: {key}");
                    let _ = &self.pddb.delete_key(&self.pddb_dict, &key, None);
                    Err(Error::from(ErrorKind::NotFound))
                }
            },
            Err(e) => Err(e),
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

fn get(pddb: &Pddb, dict: &str, key: &str) -> Result<Option<String>, Error> {
    let value = match pddb.get(dict, key, None, true, false, None, None::<fn()>) {
        Ok(mut pddb_key) => {
            let mut buffer = [0; 256];
            match pddb_key.read(&mut buffer) {
                Ok(len) => match String::from_utf8(buffer[..len].to_vec()) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        log::warn!("failed to String: {:?}", e);
                        None
                    }
                },
                Err(e) => {
                    log::warn!("failed pddb_key read: {:?}", e);
                    None
                }
            }
        }
        Err(_) => None,
    };
    log::info!("get '{}' = '{:?}'", key, value);
    Ok(value)
}

fn set(pddb: &Pddb, dict: &str, key: &str, value: Option<&str>) -> Result<(), Error> {
    log::info!("set '{}' = '{:?}'", key, value);
    // delete key first to ensure data in a prior longer key is gone
    pddb.delete_key(dict, key, None).ok();
    if let Some(value) = value {
        match pddb.get(dict, key, None, true, true, None, None::<fn()>) {
            Ok(mut pddb_key) => match pddb_key.write(&value.as_bytes()) {
                Ok(len) => {
                    pddb.sync().ok();
                    log::trace!("Wrote {} bytes to {}:{}", len, dict, key);
                }
                Err(e) => {
                    log::warn!("Error writing {}:{} {:?}", dict, key, e);
                }
            },
            Err(e) => log::warn!("failed to set pddb {}:{}  {:?}", dict, key, e),
        };
    }
    Ok(())
}
