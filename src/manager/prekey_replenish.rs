//! One-time prekey replenishment orchestrator (issue #15).
//!
//! Stateless, threshold-driven flow modeled on
//! `libsignal-service-rs::account_manager::update_pre_key_bundle`:
//!
//!   1. Ask the server how many one-time EC prekeys remain in stock for
//!      this identity (`GET /v2/keys?identity=aci`).
//!   2. If the count is at or above [`PRE_KEY_MINIMUM`], skip — no work
//!      to do.
//!   3. Otherwise, generate [`PRE_KEY_BATCH_SIZE`] fresh one-time EC
//!      prekeys, persist them locally, and upload the batch via
//!      `PUT /v2/keys?identity=aci`.
//!
//! Decisions captured in ADR 0013:
//! - ACI only. PNI replenishment is a separate follow-up; sigchat
//!   doesn't currently use a PNI session on the wire.
//! - One-time EC prekeys only. Signed-prekey rotation is time-driven,
//!   not threshold-driven, and Kyber one-time prekeys are not yet
//!   uploaded (sigchat uses last-resort Kyber only).
//! - No rollback on failed upload. Locally-persisted prekeys whose
//!   batch never reached the server are harmless: the server never
//!   advertised them in any prekey bundle, so no peer will ever ask
//!   to use them. The next replenishment cycle uses fresh IDs from
//!   the persistent counter.
//! - Triggered once per `start_receive` (i.e. once per WS session).
//!   That's enough to recover from any consume-since-last-startup
//!   without polling. Reactive replenishment (decrement-on-decrypt
//!   → trigger) is a deferred optimization.

#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]

use std::io::Error;

use url::Url;

use crate::manager::prekeys::{
    self, OneTimePreKeyJson, PRE_KEY_BATCH_SIZE, PRE_KEY_MINIMUM,
};
pub(crate) use crate::manager::rest::{OneTimePreKeyEntity, PreKeyCount, SetKeysRequest};

/// Outcome of a single replenishment attempt. Used by callers to log
/// without re-deriving state, and by tests to assert on behavior.
#[derive(Debug, PartialEq, Eq)]
pub enum ReplenishOutcome {
    /// Server reports `count` one-time prekeys remaining; threshold
    /// not crossed, so no upload was attempted.
    Skipped { server_count: u32 },
    /// `uploaded` fresh one-time prekeys were generated, persisted,
    /// and accepted by the server.
    Replenished { uploaded: u32 },
    /// Either the GET status call or the PUT upload call failed.
    /// `reason` is suitable for a single-line log message.
    Failed { reason: String },
}

/// Closure type for the GET-status leg. Production caller wraps
/// [`crate::manager::rest::get_keys_status`]; tests inject canned
/// responses without hitting the network.
pub(crate) type GetStatusFn<'a> = dyn FnMut() -> Result<PreKeyCount, Error> + 'a;
/// Closure type for the PUT-upload leg. Production caller wraps
/// [`crate::manager::rest::put_keys`].
pub(crate) type PutKeysFn<'a> = dyn FnMut(&SetKeysRequest) -> Result<(), Error> + 'a;
/// Closure type for the local generator. Production caller wraps
/// [`crate::manager::prekeys::generate_one_time_prekeys`]; tests
/// inject deterministic key IDs to keep assertions stable.
pub(crate) type GenerateFn<'a> = dyn FnMut(u32) -> Result<Vec<OneTimePreKeyJson>, Error> + 'a;

/// Pure orchestrator over injected closures. The production wrapper
/// [`replenish_aci_one_time_prekeys`] fills these in.
pub(crate) fn run_replenish(
    threshold: u32,
    batch_size: u32,
    get_status: &mut GetStatusFn<'_>,
    generate: &mut GenerateFn<'_>,
    put_keys: &mut PutKeysFn<'_>,
) -> ReplenishOutcome {
    let status = match get_status() {
        Ok(s) => s,
        Err(e) => {
            return ReplenishOutcome::Failed {
                reason: format!("get_keys_status: {e}"),
            };
        }
    };

    if status.count >= threshold {
        log::info!(
            "prekey_replenish: skip (server count={} >= threshold={})",
            status.count, threshold,
        );
        return ReplenishOutcome::Skipped { server_count: status.count };
    }

    log::info!(
        "prekey_replenish: server count={} < threshold={}, generating {} fresh one-time prekeys",
        status.count, threshold, batch_size,
    );

    let json_keys = match generate(batch_size) {
        Ok(v) => v,
        Err(e) => {
            return ReplenishOutcome::Failed {
                reason: format!("generate_one_time_prekeys: {e}"),
            };
        }
    };

    let body = SetKeysRequest {
        pre_keys: json_keys
            .into_iter()
            .map(|k| OneTimePreKeyEntity {
                key_id: k.key_id,
                public_key: k.public_key_b64url,
            })
            .collect(),
        ..SetKeysRequest::default()
    };
    let uploaded = body.pre_keys.len() as u32;

    if let Err(e) = put_keys(&body) {
        return ReplenishOutcome::Failed {
            reason: format!("put_keys: {e}"),
        };
    }

    log::info!("prekey_replenish: uploaded {} one-time prekeys", uploaded);
    ReplenishOutcome::Replenished { uploaded }
}

/// Production entry point: do a one-shot ACI one-time-prekey replenish
/// against `base_url`. Wires the live HTTP and PDDB-backed generators
/// behind the closure-driven orchestrator above. Failures are returned
/// as [`ReplenishOutcome::Failed`] rather than panicked, so the caller
/// can log and continue without disrupting the receive worker.
pub fn replenish_aci_one_time_prekeys(
    base_url: &Url,
    aci: &str,
    device_id: u32,
    password: &str,
) -> ReplenishOutcome {
    let identifier = format!("{}.{}", aci, device_id);

    let mut get_status: Box<GetStatusFn<'_>> = Box::new(|| {
        crate::manager::rest::get_keys_status(base_url, &identifier, password)
    });
    let mut generate: Box<GenerateFn<'_>> = Box::new(|n| {
        let batch = prekeys::generate_one_time_prekeys(n)?;
        Ok(batch.json)
    });
    let mut put_keys: Box<PutKeysFn<'_>> = Box::new(|body| {
        crate::manager::rest::put_keys(base_url, &identifier, password, body)
    });

    run_replenish(
        PRE_KEY_MINIMUM,
        PRE_KEY_BATCH_SIZE,
        &mut *get_status,
        &mut *generate,
        &mut *put_keys,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::ErrorKind;

    fn dummy_jsons(n: u32) -> Vec<OneTimePreKeyJson> {
        (0..n)
            .map(|i| OneTimePreKeyJson {
                key_id: 1000 + i,
                public_key_b64url: format!("pk{i}"),
            })
            .collect()
    }

    #[test]
    fn skips_when_server_count_above_threshold() {
        let put_called = RefCell::new(0u32);
        let gen_called = RefCell::new(0u32);

        let mut get_status: Box<GetStatusFn<'_>> =
            Box::new(|| Ok(PreKeyCount { count: 11, pq_count: 0 }));
        let mut generate: Box<GenerateFn<'_>> = Box::new(|n| {
            *gen_called.borrow_mut() += 1;
            Ok(dummy_jsons(n))
        });
        let mut put_keys: Box<PutKeysFn<'_>> = Box::new(|_body| {
            *put_called.borrow_mut() += 1;
            Ok(())
        });

        let outcome = run_replenish(10, 100, &mut *get_status, &mut *generate, &mut *put_keys);
        assert_eq!(outcome, ReplenishOutcome::Skipped { server_count: 11 });
        assert_eq!(*gen_called.borrow(), 0, "generate must not be called when skipping");
        assert_eq!(*put_called.borrow(), 0, "put must not be called when skipping");
    }

    #[test]
    fn skips_at_exact_threshold() {
        // Threshold is "below threshold triggers replenish" — count == threshold is fine.
        let put_called = RefCell::new(false);
        let mut get_status: Box<GetStatusFn<'_>> =
            Box::new(|| Ok(PreKeyCount { count: 10, pq_count: 0 }));
        let mut generate: Box<GenerateFn<'_>> = Box::new(|n| Ok(dummy_jsons(n)));
        let mut put_keys: Box<PutKeysFn<'_>> = Box::new(|_| {
            *put_called.borrow_mut() = true;
            Ok(())
        });
        let outcome = run_replenish(10, 100, &mut *get_status, &mut *generate, &mut *put_keys);
        assert_eq!(outcome, ReplenishOutcome::Skipped { server_count: 10 });
        assert!(!*put_called.borrow());
    }

    #[test]
    fn replenishes_when_server_count_below_threshold() {
        let put_body: RefCell<Option<SetKeysRequest>> = RefCell::new(None);
        let mut get_status: Box<GetStatusFn<'_>> =
            Box::new(|| Ok(PreKeyCount { count: 3, pq_count: 0 }));
        let mut generate: Box<GenerateFn<'_>> = Box::new(|n| Ok(dummy_jsons(n)));
        let mut put_keys: Box<PutKeysFn<'_>> = Box::new(|body| {
            *put_body.borrow_mut() = Some(SetKeysRequest {
                pre_keys: body.pre_keys.iter().map(|e| OneTimePreKeyEntity {
                    key_id: e.key_id,
                    public_key: e.public_key.clone(),
                }).collect(),
                ..SetKeysRequest::default()
            });
            Ok(())
        });

        let outcome = run_replenish(10, 100, &mut *get_status, &mut *generate, &mut *put_keys);
        assert_eq!(outcome, ReplenishOutcome::Replenished { uploaded: 100 });
        let captured = put_body.borrow();
        let captured = captured.as_ref().expect("put was called");
        assert_eq!(captured.pre_keys.len(), 100);
        assert_eq!(captured.pre_keys[0].key_id, 1000);
        assert_eq!(captured.pre_keys[99].key_id, 1099);
        assert!(captured.signed_pre_key.is_none());
        assert!(captured.pq_pre_keys.is_empty());
        assert!(captured.pq_last_resort_pre_key.is_none());
    }

    #[test]
    fn server_count_zero_triggers_replenish() {
        // The "initial fill" case — sigchat link uploads zero one-time
        // prekeys, so the very first start_receive sees count=0.
        let mut get_status: Box<GetStatusFn<'_>> =
            Box::new(|| Ok(PreKeyCount { count: 0, pq_count: 1 }));
        let mut generate: Box<GenerateFn<'_>> = Box::new(|n| Ok(dummy_jsons(n)));
        let mut put_keys: Box<PutKeysFn<'_>> = Box::new(|_| Ok(()));
        let outcome = run_replenish(10, 100, &mut *get_status, &mut *generate, &mut *put_keys);
        assert_eq!(outcome, ReplenishOutcome::Replenished { uploaded: 100 });
    }

    #[test]
    fn get_status_failure_returns_failed_without_calling_put() {
        let put_called = RefCell::new(false);
        let mut get_status: Box<GetStatusFn<'_>> =
            Box::new(|| Err(Error::new(ErrorKind::ConnectionAborted, "no route")));
        let mut generate: Box<GenerateFn<'_>> = Box::new(|n| Ok(dummy_jsons(n)));
        let mut put_keys: Box<PutKeysFn<'_>> = Box::new(|_| {
            *put_called.borrow_mut() = true;
            Ok(())
        });
        let outcome = run_replenish(10, 100, &mut *get_status, &mut *generate, &mut *put_keys);
        match outcome {
            ReplenishOutcome::Failed { reason } => {
                assert!(reason.contains("get_keys_status"), "reason: {reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(!*put_called.borrow());
    }

    #[test]
    fn generate_failure_returns_failed_without_calling_put() {
        let put_called = RefCell::new(false);
        let mut get_status: Box<GetStatusFn<'_>> =
            Box::new(|| Ok(PreKeyCount { count: 0, pq_count: 0 }));
        let mut generate: Box<GenerateFn<'_>> =
            Box::new(|_| Err(Error::new(ErrorKind::Other, "pddb wedged")));
        let mut put_keys: Box<PutKeysFn<'_>> = Box::new(|_| {
            *put_called.borrow_mut() = true;
            Ok(())
        });
        let outcome = run_replenish(10, 100, &mut *get_status, &mut *generate, &mut *put_keys);
        match outcome {
            ReplenishOutcome::Failed { reason } => {
                assert!(reason.contains("generate_one_time_prekeys"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(!*put_called.borrow());
    }

    #[test]
    fn put_failure_returns_failed_after_local_persistence() {
        // The generator was called (and would have persisted to PDDB in
        // production) before the upload failed. We do NOT roll back —
        // the orphaned local prekeys are harmless. See ADR 0013.
        let mut get_status: Box<GetStatusFn<'_>> =
            Box::new(|| Ok(PreKeyCount { count: 0, pq_count: 0 }));
        let mut generate: Box<GenerateFn<'_>> = Box::new(|n| Ok(dummy_jsons(n)));
        let mut put_keys: Box<PutKeysFn<'_>> =
            Box::new(|_| Err(Error::new(ErrorKind::Other, "503 from server")));
        let outcome = run_replenish(10, 100, &mut *get_status, &mut *generate, &mut *put_keys);
        match outcome {
            ReplenishOutcome::Failed { reason } => assert!(reason.contains("put_keys")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn batch_size_zero_still_calls_put_with_empty_body() {
        // Edge — a degenerate batch_size=0 should result in a Replenished
        // with uploaded=0. We don't enforce a minimum batch in the
        // orchestrator because the production constants make it
        // unreachable, but the behavior should be well-defined.
        let put_called = RefCell::new(false);
        let mut get_status: Box<GetStatusFn<'_>> =
            Box::new(|| Ok(PreKeyCount { count: 0, pq_count: 0 }));
        let mut generate: Box<GenerateFn<'_>> = Box::new(|n| Ok(dummy_jsons(n)));
        let mut put_keys: Box<PutKeysFn<'_>> = Box::new(|_| {
            *put_called.borrow_mut() = true;
            Ok(())
        });
        let outcome = run_replenish(10, 0, &mut *get_status, &mut *generate, &mut *put_keys);
        assert_eq!(outcome, ReplenishOutcome::Replenished { uploaded: 0 });
        assert!(*put_called.borrow());
    }
}
