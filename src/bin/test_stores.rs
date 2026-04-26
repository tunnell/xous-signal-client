//! Phase 2.3 smoke test for protocol stores against live pddb.
//!
//! Run with:
//!   cargo run --bin test_stores --features hosted
//!
//! Requires the Xous hosted environment (pddb service must be reachable via IPC).
//! Restore pddb from hosted-linked.bin before running; discard state afterward.
#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use libsignal_protocol::{
    DeviceId, IdentityKey, IdentityKeyPair, IdentityKeyStore, KyberPreKeyId, KyberPreKeyRecord,
    KyberPreKeyStore, PreKeyId, PreKeyRecord, PreKeyStore, ProtocolAddress,
    SessionStore, SignedPreKeyId, SignedPreKeyRecord, SignedPreKeyStore,
    Timestamp, GenericSignedPreKey, KeyPair, kem,
};
use rand::TryRngCore as _;
use xous_signal_client::manager::stores::{
    PddbIdentityStore, PddbKyberPreKeyStore, PddbPreKeyStore, PddbSessionStore,
    PddbSignedPreKeyStore,
};

fn main() {
    let stack_size = 1024 * 1024;
    std::thread::Builder::new()
        .stack_size(stack_size)
        .spawn(run_tests)
        .unwrap()
        .join()
        .unwrap();
}

fn run_tests() {
    log_server::init_wait().unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::info!("test_stores: starting");

    let mut passed = 0usize;
    let mut failed = 0usize;

    macro_rules! check {
        ($name:expr, $result:expr) => {{
            match $result {
                Ok(_) => {
                    log::info!("PASS: {}", $name);
                    passed += 1;
                }
                Err(e) => {
                    log::error!("FAIL: {} — {:?}", $name, e);
                    failed += 1;
                }
            }
        }};
    }

    macro_rules! assert_ok {
        ($name:expr, $expr:expr) => {{
            let r: Result<_, _> = $expr;
            check!($name, r.map(|_| ()))
        }};
    }

    let mut rng = rand::rngs::OsRng.unwrap_err();

    // ---- PreKeyStore --------------------------------------------------------
    {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        let mut store = PddbPreKeyStore::new(pddb, "sigchat.test.prekey");

        let kp = KeyPair::generate(&mut rng);
        let record = PreKeyRecord::new(PreKeyId::from(42u32), &kp);

        assert_ok!(
            "prekey/save",
            futures::executor::block_on(store.save_pre_key(PreKeyId::from(42u32), &record))
        );

        let load_result = futures::executor::block_on(store.get_pre_key(PreKeyId::from(42u32)));
        match load_result {
            Ok(loaded) => {
                if record.serialize().unwrap() == loaded.serialize().unwrap() {
                    log::info!("PASS: prekey/round-trip bytes match");
                    passed += 1;
                } else {
                    log::error!("FAIL: prekey/round-trip bytes MISMATCH");
                    failed += 1;
                }
            }
            Err(e) => {
                log::error!("FAIL: prekey/load — {:?}", e);
                failed += 1;
            }
        }

        assert_ok!(
            "prekey/remove",
            futures::executor::block_on(store.remove_pre_key(PreKeyId::from(42u32)))
        );

        let after_remove = futures::executor::block_on(store.get_pre_key(PreKeyId::from(42u32)));
        match after_remove {
            Err(libsignal_protocol::SignalProtocolError::InvalidPreKeyId) => {
                log::info!("PASS: prekey/missing-after-remove");
                passed += 1;
            }
            other => {
                log::error!("FAIL: prekey/missing-after-remove — expected InvalidPreKeyId, got {:?}", other);
                failed += 1;
            }
        }
    }

    // ---- SignedPreKeyStore --------------------------------------------------
    {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        let mut store = PddbSignedPreKeyStore::new(pddb, "sigchat.test.signed_prekey");

        let id_pair = IdentityKeyPair::generate(&mut rng);
        let kp = KeyPair::generate(&mut rng);
        let sig = id_pair.private_key()
            .calculate_signature(&kp.public_key.serialize(), &mut rng)
            .unwrap()
            .to_vec();
        let record = SignedPreKeyRecord::new(
            SignedPreKeyId::from(1u32),
            Timestamp::from_epoch_millis(0),
            &kp,
            &sig,
        );

        assert_ok!(
            "signed_prekey/save",
            futures::executor::block_on(store.save_signed_pre_key(SignedPreKeyId::from(1u32), &record))
        );

        let load_result = futures::executor::block_on(
            store.get_signed_pre_key(SignedPreKeyId::from(1u32))
        );
        match load_result {
            Ok(loaded) => {
                if record.serialize().unwrap() == loaded.serialize().unwrap() {
                    log::info!("PASS: signed_prekey/round-trip bytes match");
                    passed += 1;
                } else {
                    log::error!("FAIL: signed_prekey/round-trip bytes MISMATCH");
                    failed += 1;
                }
            }
            Err(e) => {
                log::error!("FAIL: signed_prekey/load — {:?}", e);
                failed += 1;
            }
        }
    }

    // ---- KyberPreKeyStore --------------------------------------------------
    {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        let mut store = PddbKyberPreKeyStore::new(pddb, "sigchat.test.kyber_prekey");

        let id_pair = IdentityKeyPair::generate(&mut rng);
        let record = KyberPreKeyRecord::generate(
            kem::KeyType::Kyber1024,
            KyberPreKeyId::from(1u32),
            id_pair.private_key(),
        ).unwrap();

        assert_ok!(
            "kyber_prekey/save",
            futures::executor::block_on(store.save_kyber_pre_key(KyberPreKeyId::from(1u32), &record))
        );

        let load_result = futures::executor::block_on(
            store.get_kyber_pre_key(KyberPreKeyId::from(1u32))
        );
        match load_result {
            Ok(loaded) => {
                if record.serialize().unwrap() == loaded.serialize().unwrap() {
                    log::info!("PASS: kyber_prekey/round-trip bytes match");
                    passed += 1;
                } else {
                    log::error!("FAIL: kyber_prekey/round-trip bytes MISMATCH");
                    failed += 1;
                }
            }
            Err(e) => {
                log::error!("FAIL: kyber_prekey/load — {:?}", e);
                failed += 1;
            }
        }
    }

    // ---- SessionStore: missing returns None --------------------------------
    {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        let store = PddbSessionStore::new(pddb, "sigchat.test.session");

        let addr = ProtocolAddress::new(
            "00000000-0000-0000-0000-000000000000".to_string(),
            DeviceId::new(1).unwrap(),
        );
        let result = futures::executor::block_on(store.load_session(&addr));
        match result {
            Ok(None) => {
                log::info!("PASS: session/missing-returns-none");
                passed += 1;
            }
            Ok(Some(_)) => {
                log::error!("FAIL: session/missing-returns-none — got Some(_)");
                failed += 1;
            }
            Err(e) => {
                log::error!("FAIL: session/missing-returns-none — got Err({:?})", e);
                failed += 1;
            }
        }
    }

    // ---- IdentityStore: peer TOFU round-trip --------------------------------
    {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        let mut store = PddbIdentityStore::new(pddb, "sigchat.account", "sigchat.test.identity");

        let kp = KeyPair::generate(&mut rng);
        let identity = IdentityKey::new(kp.public_key);
        let addr = ProtocolAddress::new(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
            DeviceId::new(2).unwrap(),
        );

        // first use: save identity
        let change = futures::executor::block_on(
            store.save_identity(&addr, &identity)
        );
        match change {
            Ok(_) => {
                log::info!("PASS: identity/save-new");
                passed += 1;
            }
            Err(e) => {
                log::error!("FAIL: identity/save-new — {:?}", e);
                failed += 1;
            }
        }

        // load it back
        let get_result = futures::executor::block_on(store.get_identity(&addr));
        match get_result {
            Ok(Some(loaded)) => {
                if identity.serialize() == loaded.serialize() {
                    log::info!("PASS: identity/round-trip bytes match");
                    passed += 1;
                } else {
                    log::error!("FAIL: identity/round-trip bytes MISMATCH");
                    failed += 1;
                }
            }
            other => {
                log::error!("FAIL: identity/get — got {:?}", other);
                failed += 1;
            }
        }

        // TOFU: is_trusted after saving should return true
        let trusted = futures::executor::block_on(
            store.is_trusted_identity(&addr, &identity, libsignal_protocol::Direction::Receiving)
        );
        match trusted {
            Ok(true) => {
                log::info!("PASS: identity/trusted-after-save");
                passed += 1;
            }
            other => {
                log::error!("FAIL: identity/trusted-after-save — got {:?}", other);
                failed += 1;
            }
        }
    }

    // ---- IdentityStore: get_identity_key_pair (reads sigchat.account) ------
    // Only run if sigchat.account has been written (i.e. device is linked).
    {
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        let store = PddbIdentityStore::new(pddb, "sigchat.account", "sigchat.identity");

        let result = futures::executor::block_on(store.get_identity_key_pair());
        match result {
            Ok(kp) => {
                // Sanity: serializing and deserializing the keypair should round-trip.
                let pub_bytes = kp.identity_key().serialize();
                let re_decoded = IdentityKey::decode(&pub_bytes);
                match re_decoded {
                    Ok(_) => {
                        log::info!("PASS: identity/get_identity_key_pair (linked account)");
                        passed += 1;
                    }
                    Err(e) => {
                        log::error!("FAIL: identity/keypair-serialize-roundtrip — {:?}", e);
                        failed += 1;
                    }
                }
            }
            Err(e) => {
                log::warn!("SKIP: identity/get_identity_key_pair — account not linked or key missing: {:?}", e);
                // Not a hard failure — device may not be linked yet.
            }
        }
    }

    // ---- Summary -----------------------------------------------------------
    log::info!("test_stores: {} passed, {} failed", passed, failed);
    if failed > 0 {
        log::error!("test_stores: FAILED");
        xous::terminate_process(1)
    } else {
        log::info!("test_stores: ALL PASSED");
        xous::terminate_process(0)
    }
}
