// PDDB-backed implementations of libsignal_protocol store traits.
// Phase 1: skeletons only — all methods return unimplemented!("phase 2").
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]

use async_trait::async_trait;
use std::io::{Read, Write};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use libsignal_protocol::{
    Direction, IdentityChange, IdentityKey, IdentityKeyPair, IdentityKeyStore,
    GenericSignedPreKey,
    KyberPreKeyId, KyberPreKeyRecord, KyberPreKeyStore,
    PreKeyId, PreKeyRecord, PreKeyStore,
    PrivateKey, ProtocolAddress, PublicKey, SignalProtocolError,
    SessionRecord, SessionStore,
    SignedPreKeyId, SignedPreKeyRecord, SignedPreKeyStore,
};

type SignalResult<T> = std::result::Result<T, SignalProtocolError>;

// ---------------------------------------------------------------------------
// PddbIdentityStore
// ---------------------------------------------------------------------------

pub struct PddbIdentityStore {
    pddb: pddb::Pddb,
    account_dict: &'static str,
    identity_dict: &'static str,
}

impl PddbIdentityStore {
    pub fn new(pddb: pddb::Pddb, account_dict: &'static str, identity_dict: &'static str) -> Self {
        Self { pddb, account_dict, identity_dict }
    }
}

fn read_account_string(pddb: &pddb::Pddb, dict: &str, key: &str) -> Option<String> {
    match pddb.get(dict, key, None, true, false, None, None::<fn()>) {
        Ok(mut handle) => {
            let mut buf = [0u8; 256];
            match handle.read(&mut buf) {
                Ok(n) => String::from_utf8(buf[..n].to_vec()).ok(),
                Err(_) => None,
            }
        }
        Err(_) => None,
    }
}

#[async_trait(?Send)]
impl IdentityKeyStore for PddbIdentityStore {
    async fn get_identity_key_pair(&self) -> SignalResult<IdentityKeyPair> {
        let not_found = || SignalProtocolError::InvalidState(
            "pddb",
            "aci identity key not found".to_string(),
        );
        let priv_b64 = read_account_string(&self.pddb, self.account_dict, "aci.identity.private")
            .ok_or_else(not_found)?;
        let pub_b64 = read_account_string(&self.pddb, self.account_dict, "aci.identity.public")
            .ok_or_else(not_found)?;
        let priv_bytes = URL_SAFE_NO_PAD.decode(priv_b64.trim())
            .map_err(|e| SignalProtocolError::InvalidState("pddb", format!("priv base64: {e}")))?;
        let pub_bytes = URL_SAFE_NO_PAD.decode(pub_b64.trim())
            .map_err(|e| SignalProtocolError::InvalidState("pddb", format!("pub base64: {e}")))?;
        let private_key = PrivateKey::deserialize(&priv_bytes)?;
        let identity_key = IdentityKey::decode(&pub_bytes)?;
        Ok(IdentityKeyPair::new(identity_key, private_key))
    }

    async fn get_local_registration_id(&self) -> SignalResult<u32> {
        read_account_string(&self.pddb, self.account_dict, "registration_id")
            .and_then(|s| s.trim().parse::<u32>().ok())
            .ok_or_else(|| SignalProtocolError::InvalidState(
                "pddb",
                "registration_id not found".to_string(),
            ))
    }

    async fn save_identity(
        &mut self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
    ) -> SignalResult<IdentityChange> {
        let key = format!("{}.{}", address.name(), address.device_id());
        match pddb_read_binary(&self.pddb, self.identity_dict, &key) {
            Ok(existing_bytes) => {
                match IdentityKey::decode(&existing_bytes) {
                    Ok(existing) if existing == *identity => {
                        Ok(IdentityChange::NewOrUnchanged)
                    }
                    _ => {
                        // Different key or decode failed — overwrite
                        let data = identity.serialize();
                        pddb_write_binary(&self.pddb, self.identity_dict, &key, &data)
                            .map_err(|e| io_err_to_signal(e, "write failed"))?;
                        Ok(IdentityChange::ReplacedExisting)
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let data = identity.serialize();
                pddb_write_binary(&self.pddb, self.identity_dict, &key, &data)
                    .map_err(|e| io_err_to_signal(e, "write failed"))?;
                Ok(IdentityChange::NewOrUnchanged)
            }
            Err(e) => Err(io_err_to_signal(e, "read failed")),
        }
    }

    async fn is_trusted_identity(
        &self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
        _direction: Direction,
    ) -> SignalResult<bool> {
        let key = format!("{}.{}", address.name(), address.device_id());
        match pddb_read_binary(&self.pddb, self.identity_dict, &key) {
            Ok(existing_bytes) => {
                match IdentityKey::decode(&existing_bytes) {
                    Ok(existing) => Ok(existing == *identity),
                    Err(e) => Err(e),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(e) => Err(io_err_to_signal(e, "read failed")),
        }
    }

    async fn get_identity(&self, address: &ProtocolAddress) -> SignalResult<Option<IdentityKey>> {
        let key = format!("{}.{}", address.name(), address.device_id());
        match pddb_read_binary(&self.pddb, self.identity_dict, &key) {
            Ok(buf) => IdentityKey::decode(&buf).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err_to_signal(e, "read failed")),
        }
    }
}

// ---------------------------------------------------------------------------
// PddbPreKeyStore
// ---------------------------------------------------------------------------

pub struct PddbPreKeyStore {
    pddb: pddb::Pddb,
    dict: &'static str,
}

impl PddbPreKeyStore {
    pub fn new(pddb: pddb::Pddb, dict: &'static str) -> Self {
        Self { pddb, dict }
    }
}

fn pddb_read_binary(pddb: &pddb::Pddb, dict: &str, key: &str) -> std::io::Result<Vec<u8>> {
    let mut handle = pddb.get(dict, key, None, true, false, None, None::<fn()>)?;
    let mut buf = Vec::new();
    handle.read_to_end(&mut buf)?;
    Ok(buf)
}

fn pddb_write_binary(pddb: &pddb::Pddb, dict: &str, key: &str, data: &[u8]) -> std::io::Result<()> {
    pddb.delete_key(dict, key, None).ok();
    let mut handle = pddb.get(dict, key, None, true, true, None, None::<fn()>)?;
    handle.write_all(data)?;
    pddb.sync().ok();
    Ok(())
}

fn io_err_to_signal(e: std::io::Error, context: &str) -> SignalProtocolError {
    SignalProtocolError::InvalidState("pddb", format!("{context}: {e}"))
}

#[async_trait(?Send)]
impl PreKeyStore for PddbPreKeyStore {
    async fn get_pre_key(&self, prekey_id: PreKeyId) -> SignalResult<PreKeyRecord> {
        let key = format!("{}", u32::from(prekey_id));
        match pddb_read_binary(&self.pddb, self.dict, &key) {
            Ok(buf) => PreKeyRecord::deserialize(&buf),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(SignalProtocolError::InvalidPreKeyId)
            }
            Err(e) => Err(io_err_to_signal(e, "read failed")),
        }
    }

    async fn save_pre_key(&mut self, prekey_id: PreKeyId, record: &PreKeyRecord) -> SignalResult<()> {
        let key = format!("{}", u32::from(prekey_id));
        let buf = record.serialize()
            .map_err(|e| SignalProtocolError::InvalidState("pddb", format!("serialize failed: {e}")))?;
        pddb_write_binary(&self.pddb, self.dict, &key, &buf)
            .map_err(|e| io_err_to_signal(e, "write failed"))?;
        Ok(())
    }

    async fn remove_pre_key(&mut self, prekey_id: PreKeyId) -> SignalResult<()> {
        let key = format!("{}", u32::from(prekey_id));
        match self.pddb.delete_key(self.dict, &key, None) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err_to_signal(e, "delete failed")),
        }
    }
}

// ---------------------------------------------------------------------------
// PddbSignedPreKeyStore
// ---------------------------------------------------------------------------

pub struct PddbSignedPreKeyStore {
    pddb: pddb::Pddb,
    dict: &'static str,
}

impl PddbSignedPreKeyStore {
    pub fn new(pddb: pddb::Pddb, dict: &'static str) -> Self {
        Self { pddb, dict }
    }
}

#[async_trait(?Send)]
impl SignedPreKeyStore for PddbSignedPreKeyStore {
    async fn get_signed_pre_key(
        &self,
        signed_prekey_id: SignedPreKeyId,
    ) -> SignalResult<SignedPreKeyRecord> {
        let key = format!("{}", u32::from(signed_prekey_id));
        match pddb_read_binary(&self.pddb, self.dict, &key) {
            Ok(buf) => SignedPreKeyRecord::deserialize(&buf),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(SignalProtocolError::InvalidSignedPreKeyId)
            }
            Err(e) => Err(io_err_to_signal(e, "read failed")),
        }
    }

    async fn save_signed_pre_key(
        &mut self,
        signed_prekey_id: SignedPreKeyId,
        record: &SignedPreKeyRecord,
    ) -> SignalResult<()> {
        let key = format!("{}", u32::from(signed_prekey_id));
        let buf = record.serialize()?;
        pddb_write_binary(&self.pddb, self.dict, &key, &buf)
            .map_err(|e| io_err_to_signal(e, "write failed"))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PddbKyberPreKeyStore
// ---------------------------------------------------------------------------

pub struct PddbKyberPreKeyStore {
    pddb: pddb::Pddb,
    dict: &'static str,
}

impl PddbKyberPreKeyStore {
    pub fn new(pddb: pddb::Pddb, dict: &'static str) -> Self {
        Self { pddb, dict }
    }
}

#[async_trait(?Send)]
impl KyberPreKeyStore for PddbKyberPreKeyStore {
    async fn get_kyber_pre_key(&self, kyber_prekey_id: KyberPreKeyId) -> SignalResult<KyberPreKeyRecord> {
        let key = format!("{}", u32::from(kyber_prekey_id));
        match pddb_read_binary(&self.pddb, self.dict, &key) {
            Ok(buf) => KyberPreKeyRecord::deserialize(&buf),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(SignalProtocolError::InvalidKyberPreKeyId)
            }
            Err(e) => Err(io_err_to_signal(e, "read failed")),
        }
    }

    async fn save_kyber_pre_key(
        &mut self,
        kyber_prekey_id: KyberPreKeyId,
        record: &KyberPreKeyRecord,
    ) -> SignalResult<()> {
        let key = format!("{}", u32::from(kyber_prekey_id));
        let buf = record.serialize()?;
        pddb_write_binary(&self.pddb, self.dict, &key, &buf)
            .map_err(|e| io_err_to_signal(e, "write failed"))?;
        Ok(())
    }

    async fn mark_kyber_pre_key_used(
        &mut self,
        kyber_prekey_id: KyberPreKeyId,
        ec_prekey_id: SignedPreKeyId,
        base_key: &PublicKey,
    ) -> SignalResult<()> {
        // sigchat currently only uploads last-resort Kyber pre-keys (see
        // manager/prekeys.rs::generate_kyber_last_resort), so every call lands
        // on libsignal's "last-resort" path:
        //   * do NOT delete the key
        //   * reject reuse of the same (kyber_id, ec_prekey_id, base_key) tuple
        // If sigchat ever starts uploading one-time Kyber pre-keys, this store
        // will need a way to tell them apart and delete one-time keys on first use.
        let base_pk_b64 = URL_SAFE_NO_PAD.encode(base_key.serialize());
        let dedup_key = format!(
            "used:{}:{}:{}",
            u32::from(kyber_prekey_id),
            u32::from(ec_prekey_id),
            base_pk_b64,
        );
        match pddb_read_binary(&self.pddb, self.dict, &dedup_key) {
            Ok(_) => {
                log::warn!(
                    "kyber prekey reuse rejected: kyber={} ec={}",
                    u32::from(kyber_prekey_id),
                    u32::from(ec_prekey_id),
                );
                Err(SignalProtocolError::InvalidKyberPreKeyId)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                pddb_write_binary(&self.pddb, self.dict, &dedup_key, &[1u8])
                    .map_err(|e| io_err_to_signal(e, "mark used: write failed"))?;
                log::debug!(
                    "kyber last-resort key {} marked used (ec={})",
                    u32::from(kyber_prekey_id),
                    u32::from(ec_prekey_id),
                );
                Ok(())
            }
            Err(e) => Err(io_err_to_signal(e, "mark used: read failed")),
        }
    }
}

// ---------------------------------------------------------------------------
// PddbSessionStore
// ---------------------------------------------------------------------------

pub struct PddbSessionStore {
    pddb: pddb::Pddb,
    dict: &'static str,
}

impl PddbSessionStore {
    pub fn new(pddb: pddb::Pddb, dict: &'static str) -> Self {
        Self { pddb, dict }
    }

    /// Drop the session record for `address`. Used by the send path on 409
    /// (extra devices) and 410 (stale devices) so re-encryption of the next
    /// attempt does not target a device the server has reported gone or
    /// changed. Missing key on delete is treated as success.
    pub fn delete_session(&self, address: &ProtocolAddress) {
        let key = format!("{}.{}", address.name(), address.device_id());
        let _ = self.pddb.delete_key(self.dict, &key, None);
    }
}

#[async_trait(?Send)]
impl SessionStore for PddbSessionStore {
    async fn load_session(&self, address: &ProtocolAddress) -> SignalResult<Option<SessionRecord>> {
        let key = format!("{}.{}", address.name(), address.device_id());
        match pddb_read_binary(&self.pddb, self.dict, &key) {
            Ok(buf) => SessionRecord::deserialize(&buf).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err_to_signal(e, "read failed")),
        }
    }

    async fn store_session(
        &mut self,
        address: &ProtocolAddress,
        record: &SessionRecord,
    ) -> SignalResult<()> {
        let key = format!("{}.{}", address.name(), address.device_id());
        let buf = record.serialize()?;
        pddb_write_binary(&self.pddb, self.dict, &key, &buf)
            .map_err(|e| io_err_to_signal(e, "write failed"))?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use libsignal_protocol::{DeviceId, KeyPair, PrivateKey};
    use rand::TryRngCore as _;

    fn test_prekey_store() -> PddbPreKeyStore {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        PddbPreKeyStore::new(pddb, "sigchat.test.prekey")
    }

    #[test]
    #[ignore = "requires Xous IPC server (run via cargo xtask run)"]
    fn prekey_round_trip() {
        // generate a prekey record
        let mut rng = rand::rngs::OsRng.unwrap_err();
        let kp = KeyPair::generate(&mut rng);
        let record = PreKeyRecord::new(PreKeyId::from(1u32), &kp);

        let mut store = test_prekey_store();
        // save
        futures::executor::block_on(store.save_pre_key(PreKeyId::from(1u32), &record))
            .expect("save should succeed");
        // load back
        let loaded = futures::executor::block_on(store.get_pre_key(PreKeyId::from(1u32)))
            .expect("get should succeed");
        assert_eq!(
            record.serialize().unwrap(),
            loaded.serialize().unwrap()
        );
        // cleanup
        futures::executor::block_on(store.remove_pre_key(PreKeyId::from(1u32)))
            .expect("remove should succeed");
    }

    #[test]
    #[ignore = "requires Xous IPC server (run via cargo xtask run)"]
    fn prekey_missing_returns_invalid_id() {
        let store = test_prekey_store();
        let result = futures::executor::block_on(store.get_pre_key(PreKeyId::from(99999u32)));
        assert!(matches!(result, Err(SignalProtocolError::InvalidPreKeyId)));
    }

    fn test_signed_prekey_store() -> PddbSignedPreKeyStore {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        PddbSignedPreKeyStore::new(pddb, "sigchat.test.signed_prekey")
    }

    #[test]
    #[ignore = "requires Xous IPC server (run via cargo xtask run)"]
    fn signed_prekey_round_trip() {
        use libsignal_protocol::{KeyPair, Timestamp};
        let mut rng = rand::rngs::OsRng.unwrap_err();
        let kp = KeyPair::generate(&mut rng);
        // sign the public key bytes with the private key to produce a valid signature
        let pub_key_bytes = kp.public_key.serialize();
        let signature = kp.private_key.calculate_signature(&pub_key_bytes, &mut rng)
            .expect("signature should succeed");
        let record = SignedPreKeyRecord::new(
            SignedPreKeyId::from(1u32),
            Timestamp::from_epoch_millis(0),
            &kp,
            &signature,
        );

        let mut store = test_signed_prekey_store();
        futures::executor::block_on(store.save_signed_pre_key(SignedPreKeyId::from(1u32), &record))
            .expect("save should succeed");
        let loaded = futures::executor::block_on(store.get_signed_pre_key(SignedPreKeyId::from(1u32)))
            .expect("get should succeed");
        assert_eq!(record.serialize().unwrap(), loaded.serialize().unwrap());
    }

    #[test]
    #[ignore = "requires Xous IPC server (run via cargo xtask run)"]
    fn signed_prekey_missing_returns_invalid_id() {
        let store = test_signed_prekey_store();
        let result = futures::executor::block_on(
            store.get_signed_pre_key(SignedPreKeyId::from(88888u32))
        );
        assert!(matches!(result, Err(SignalProtocolError::InvalidSignedPreKeyId)));
    }

    fn test_kyber_prekey_store() -> PddbKyberPreKeyStore {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        PddbKyberPreKeyStore::new(pddb, "sigchat.test.kyber_prekey")
    }

    #[test]
    #[ignore = "requires Xous IPC server (run via cargo xtask run)"]
    fn kyber_prekey_round_trip() {
        use libsignal_protocol::{IdentityKeyPair, kem};
        let mut rng = rand::rngs::OsRng.unwrap_err();
        let id_pair = IdentityKeyPair::generate(&mut rng);
        let record = KyberPreKeyRecord::generate(
            kem::KeyType::Kyber1024,
            KyberPreKeyId::from(1u32),
            id_pair.private_key(),
        ).expect("generate should succeed");

        let mut store = test_kyber_prekey_store();
        futures::executor::block_on(store.save_kyber_pre_key(KyberPreKeyId::from(1u32), &record))
            .expect("save should succeed");
        let loaded = futures::executor::block_on(store.get_kyber_pre_key(KyberPreKeyId::from(1u32)))
            .expect("get should succeed");
        assert_eq!(record.serialize().unwrap(), loaded.serialize().unwrap());
    }

    #[test]
    #[ignore = "requires Xous IPC server (run via cargo xtask run)"]
    fn kyber_prekey_missing_returns_error() {
        let store = test_kyber_prekey_store();
        let result = futures::executor::block_on(
            store.get_kyber_pre_key(KyberPreKeyId::from(77777u32))
        );
        // just assert it's an Err — the variant name will be confirmed by running the test
        assert!(result.is_err());
    }

    fn test_session_store() -> PddbSessionStore {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        PddbSessionStore::new(pddb, "sigchat.test.session")
    }

    #[test]
    #[ignore = "requires Xous IPC server (run via cargo xtask run)"]
    fn session_missing_returns_none() {
        let store = test_session_store();
        let addr = ProtocolAddress::new("test-peer-missing".to_string(), DeviceId::new(1).unwrap());
        let result = futures::executor::block_on(store.load_session(&addr))
            .expect("load_session should not error for missing session");
        assert!(result.is_none());
    }

    fn test_identity_store() -> PddbIdentityStore {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        PddbIdentityStore::new(pddb, "sigchat.account", "sigchat.test.identity")
    }

    #[test]
    #[ignore = "requires Xous IPC server (run via cargo xtask run)"]
    fn identity_missing_returns_none() {
        let store = test_identity_store();
        let addr = ProtocolAddress::new("test-peer-identity-missing".to_string(), DeviceId::new(1).unwrap());
        let result = futures::executor::block_on(store.get_identity(&addr))
            .expect("get_identity should not error for missing peer");
        assert!(result.is_none());
    }

    #[test]
    #[ignore = "requires Xous IPC server (run via cargo xtask run)"]
    fn identity_tofu_first_use_trusted() {
        let mut rng = rand::rngs::OsRng.unwrap_err();
        let kp = KeyPair::generate(&mut rng);
        let identity = IdentityKey::new(kp.public_key);
        let addr = ProtocolAddress::new("test-peer-tofu".to_string(), DeviceId::new(1).unwrap());
        let store = test_identity_store();
        let trusted = futures::executor::block_on(
            store.is_trusted_identity(&addr, &identity, Direction::Receiving)
        ).expect("is_trusted_identity should succeed");
        assert!(trusted, "first use should be trusted (TOFU)");
    }
}
