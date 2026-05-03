//! Cooperative cancellation for long-running operations in
//! `Manager::link()`.
//!
//! `CancellationToken` owns the source-of-truth atomic flag.
//! `CancellationHandle` is a cheap clone the operation polls. The
//! flag is one-way: once set, it stays set.
//!
//! `FlapWatcher` glues a `WifiObserver` to a callback that fires on
//! `Connected → !Connected` transitions. The intended callback flips
//! a `CancellationHandle` (and may also poke a per-operation cancel
//! mechanism — e.g. the existing `SignalWsServer::cancel()` IPC).
//!
//! See `_open-followups/2026-05-02-wifi-state-api-reference.md` for
//! the broader API context.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use com_rs::LinkState;

use crate::manager::wifi_observer::{WifiObserver, WifiState};

/// Source-of-truth cancellation flag. One-way: once cancelled, stays
/// cancelled. Distribute cheap `CancellationHandle` clones to worker
/// threads or callbacks; any handle can flip the flag.
#[derive(Debug)]
pub struct CancellationToken {
    inner: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self { inner: Arc::new(AtomicBool::new(false)) }
    }

    pub fn cancel(&self) {
        self.inner.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.load(Ordering::SeqCst)
    }

    /// Cheap cloneable handle suitable for passing into worker threads
    /// or callbacks.
    pub fn handle(&self) -> CancellationHandle {
        CancellationHandle { inner: self.inner.clone() }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Cloneable handle to a `CancellationToken`. Cloning is just an Arc
/// bump.
#[derive(Clone, Debug)]
pub struct CancellationHandle {
    inner: Arc<AtomicBool>,
}

impl CancellationHandle {
    pub fn cancel(&self) {
        self.inner.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.load(Ordering::SeqCst)
    }
}

/// Fires a callback when a `WifiObserver` reports a
/// `Connected → !Connected` transition.
///
/// Lifetime: `FlapWatcher::new` registers a listener on the observer.
/// Listeners on a `WifiObserver` are not removable individually
/// (demo-grade scope). Dropping a `FlapWatcher` is a no-op — the
/// listener stays in the observer's list. The callback should be
/// idempotent (e.g. `AtomicBool::store(true)`); duplicate invocations
/// from a long-lived observer accumulating watchers across retries
/// are by design no-ops.
///
/// The callback runs on the observer's worker thread, so keep it
/// cheap and non-blocking.
pub struct FlapWatcher {
    /// Marker. The actual lifetime is owned by the observer's
    /// listener vec.
    _marker: (),
}

impl FlapWatcher {
    /// Register a flap callback on `observer`. The callback fires
    /// exactly when `link_state` transitions from `Connected` to any
    /// non-`Connected` state.
    pub fn new<F>(observer: &WifiObserver, on_flap: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        // Seed `last` with the observer's current link_state so that
        // a state-change-from-Connected that happens between observer
        // creation and FlapWatcher creation is not lost.
        let last = Arc::new(Mutex::new(observer.current().link_state));
        observer.subscribe(move |new: &WifiState| {
            let mut prev = last.lock().unwrap();
            let was = *prev;
            *prev = new.link_state;
            if was == LinkState::Connected && new.link_state != LinkState::Connected {
                on_flap();
            }
        });
        Self { _marker: () }
    }

    /// Convenience constructor that wires the flap callback to a
    /// `CancellationHandle::cancel` invocation.
    pub fn for_handle(observer: &WifiObserver, handle: CancellationHandle) -> Self {
        Self::new(observer, move || handle.cancel())
    }
}

// ============================ tests ============================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::thread;
    use std::time::Duration;

    fn wait_for<F: Fn() -> bool>(predicate: F, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        predicate()
    }

    #[test]
    fn token_starts_uncancelled() {
        let t = CancellationToken::new();
        assert!(!t.is_cancelled());
        assert!(!t.handle().is_cancelled());
    }

    #[test]
    fn cancel_visible_via_token_and_handle() {
        let t = CancellationToken::new();
        let h = t.handle();
        assert!(!t.is_cancelled());
        assert!(!h.is_cancelled());
        t.cancel();
        assert!(t.is_cancelled());
        assert!(h.is_cancelled());
    }

    #[test]
    fn handle_cancel_visible_via_token() {
        let t = CancellationToken::new();
        let h = t.handle();
        h.cancel();
        assert!(t.is_cancelled());
    }

    #[test]
    fn cancel_visible_across_threads() {
        let t = CancellationToken::new();
        let h = t.handle();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            h.cancel();
        });
        let observer_thread = thread::spawn({
            let h2 = t.handle();
            move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(1);
                while std::time::Instant::now() < deadline {
                    if h2.is_cancelled() {
                        return true;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                false
            }
        });
        canceller.join().unwrap();
        assert!(observer_thread.join().unwrap(), "observer thread should see cancellation");
    }

    #[test]
    fn flap_watcher_cancels_on_connected_to_disconnected() {
        let obs = WifiObserver::for_test();
        obs.inject_for_test(LinkState::Connected);
        let t = CancellationToken::new();
        let _w = FlapWatcher::for_handle(&obs, t.handle());
        assert!(!t.is_cancelled());
        obs.inject_for_test(LinkState::Disconnected);
        assert!(wait_for(|| t.is_cancelled(), Duration::from_millis(200)));
    }

    #[test]
    fn flap_watcher_cancels_on_connected_to_any_non_connected() {
        for terminal in [
            LinkState::Disconnected,
            LinkState::Connecting,
            LinkState::WFXError,
            LinkState::Initializing,
            LinkState::ResetHold,
            LinkState::Uninitialized,
            LinkState::Unknown,
        ] {
            let obs = WifiObserver::for_test();
            obs.inject_for_test(LinkState::Connected);
            let t = CancellationToken::new();
            let _w = FlapWatcher::for_handle(&obs, t.handle());
            obs.inject_for_test(terminal);
            assert!(
                wait_for(|| t.is_cancelled(), Duration::from_millis(200)),
                "expected cancel after Connected -> {:?}",
                terminal
            );
        }
    }

    #[test]
    fn flap_watcher_does_not_cancel_on_repeated_connected() {
        let obs = WifiObserver::for_test();
        obs.inject_for_test(LinkState::Connected);
        let t = CancellationToken::new();
        let _w = FlapWatcher::for_handle(&obs, t.handle());
        // Repeated Connected events should be no-ops.
        obs.inject_for_test(LinkState::Connected);
        obs.inject_for_test(LinkState::Connected);
        thread::sleep(Duration::from_millis(50));
        assert!(!t.is_cancelled());
    }

    #[test]
    fn flap_watcher_does_not_cancel_when_starting_disconnected() {
        // Unknown / Disconnected → Connecting → Connected: no flap.
        let obs = WifiObserver::for_test();
        let t = CancellationToken::new();
        let _w = FlapWatcher::for_handle(&obs, t.handle());
        obs.inject_for_test(LinkState::Connecting);
        obs.inject_for_test(LinkState::Connected);
        thread::sleep(Duration::from_millis(50));
        assert!(!t.is_cancelled());
    }

    #[test]
    fn flap_watcher_does_not_fire_on_unknown_to_connected_then_disconnect_fires() {
        // iter-A.2.3 regression guard: WifiObserver::new dropped the
        // com.wlan_sync_state() seed call (services/net/src/lib.rs:102-104
        // documents COM congestion under direct calls). Initial state is
        // now LinkState::Unknown until the first WifiStateCallback
        // broadcast fires. The doc comment on WifiObserver::new claims
        // "FlapWatcher's transition logic is safe under Unknown init"
        // because the Connected→!Connected check rules out an
        // Unknown→Connected first transition firing on_flap.
        //
        // This test makes that claim explicit and durable: starting
        // Unknown, an Unknown→Connected broadcast must NOT fire on_flap;
        // a subsequent Connected→Disconnected MUST; and a subsequent
        // Disconnected→Connected must NOT.
        let obs = WifiObserver::for_test();
        // for_test() initializes link_state to Unknown via WifiState::unknown().
        let count = Arc::new(AtomicUsize::new(0));
        let count_for_cb = count.clone();
        let _w = FlapWatcher::new(&obs, move || {
            count_for_cb.fetch_add(1, Ordering::SeqCst);
        });
        // 1. Unknown → Connected: no flap (the iter-A.2.3 worry).
        obs.inject_for_test(LinkState::Connected);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "Unknown → Connected must not fire on_flap"
        );
        // 2. Connected → Disconnected: flap fires.
        obs.inject_for_test(LinkState::Disconnected);
        assert!(
            wait_for(
                || count.load(Ordering::SeqCst) == 1,
                Duration::from_millis(200)
            ),
            "Connected → Disconnected must fire on_flap exactly once"
        );
        // 3. Disconnected → Connected: must NOT fire (back-to-good is
        //    not a flap).
        obs.inject_for_test(LinkState::Connected);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "Disconnected → Connected must not fire on_flap (back-to-good is not a flap)"
        );
    }

    #[test]
    fn flap_watcher_callback_form_invokes_arbitrary_closure() {
        let obs = WifiObserver::for_test();
        obs.inject_for_test(LinkState::Connected);
        let count = Arc::new(AtomicUsize::new(0));
        let count_for_cb = count.clone();
        let _w = FlapWatcher::new(&obs, move || {
            count_for_cb.fetch_add(1, Ordering::SeqCst);
        });
        obs.inject_for_test(LinkState::Disconnected);
        assert!(wait_for(
            || count.load(Ordering::SeqCst) == 1,
            Duration::from_millis(200)
        ));
        // No further flaps from non-Connected states; counter stays 1.
        obs.inject_for_test(LinkState::Connecting);
        obs.inject_for_test(LinkState::Disconnected);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn flap_watcher_re_arms_after_reconnect() {
        // Connected -> Disconnected (cancel #1)
        // Disconnected -> Connected
        // Connected -> Disconnected (cancel #2 if re-armed)
        // We re-arm by creating a fresh FlapWatcher after reconnect;
        // the old one is still listening but its CancellationHandle
        // is the old token (already cancelled).
        let obs = WifiObserver::for_test();
        obs.inject_for_test(LinkState::Connected);

        let t1 = CancellationToken::new();
        let _w1 = FlapWatcher::for_handle(&obs, t1.handle());
        obs.inject_for_test(LinkState::Disconnected);
        assert!(wait_for(|| t1.is_cancelled(), Duration::from_millis(200)));

        obs.inject_for_test(LinkState::Connected);
        let t2 = CancellationToken::new();
        let _w2 = FlapWatcher::for_handle(&obs, t2.handle());
        obs.inject_for_test(LinkState::Disconnected);
        assert!(wait_for(|| t2.is_cancelled(), Duration::from_millis(200)));
    }
}
