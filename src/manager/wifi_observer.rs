//! WiFi state observer for cooperative flap recovery.
//!
//! Subscribes to the net service's WifiStateCallback broadcast and
//! maintains a thread-safe snapshot of the current `LinkState`. Long-
//! running operations in `Manager::link()` consult this snapshot (and
//! associated cancellation token, see `cancellation.rs` and the
//! `FlapWatcher`) to abort early when the WiFi link drops, instead of
//! waiting on TCP RTO.
//!
//! Demo-grade scope per `_handoffs/2026-05-02-wifi-observer-handoff.md`:
//! only `link_state` is tracked. The broadcast does not carry
//! `DhcpState`; queueing an extra IPC per state change to fetch it is
//! the exact pattern bug #2 traced (FastSpace pressure under dense
//! IPC), so we don't. DHCP visibility is a production-hardening item.
//!
//! See `_open-followups/2026-05-02-wifi-state-api-reference.md` for
//! the API surface this builds on.
//!
//! Test mode: `WifiObserver::for_test()` constructs an observer with
//! no IPC subscription, suitable for `cargo test --lib` (no Xous
//! server harness) and for hosted-mode integration tests that need to
//! drive synthetic state transitions via `inject_for_test`.

use std::io;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use com::api::WlanStatusIpc;
use com_rs::LinkState;
use net::NetManager;
use xous::{CID, SID};
use xous_ipc::Buffer;

/// Opcode used by the broadcast forwarder when re-lending us a
/// WlanStatusIpc. Any u32 works (the value is what we pass to
/// `wifi_state_subscribe`); a stable named constant aids readability.
const UPDATE_OPCODE: u32 = 0;

/// Opcode the observer sends to its own SID on Drop to wake the
/// worker out of `xous::receive_message`. Distinct from
/// `UPDATE_OPCODE` so the worker can route correctly.
const SHUTDOWN_OPCODE: u32 = 1;

/// Snapshot of WiFi state observed via the net broadcast.
///
/// `last_change` is set to `Instant::now()` on every received broadcast
/// regardless of whether `link_state` itself changed. Strictly monotonic
/// per observer (the worker rejects out-of-order updates).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiState {
    pub link_state: LinkState,
    pub last_change: Instant,
}

impl WifiState {
    pub fn unknown() -> Self {
        Self { link_state: LinkState::Unknown, last_change: Instant::now() }
    }

    pub fn is_connected(&self) -> bool {
        self.link_state == LinkState::Connected
    }
}

/// Listener callback registered via `WifiObserver::subscribe`. Runs on
/// the observer's worker thread; should be cheap and non-blocking.
pub type Listener = Arc<dyn Fn(&WifiState) + Send + Sync + 'static>;

struct Inner {
    state: RwLock<WifiState>,
    /// Companion mutex + condvar for `wait_for_connection`. The mutex
    /// guards no real state — it's the wait-companion required by
    /// `Condvar::wait_timeout`.
    waiter: (Mutex<()>, Condvar),
    listeners: Mutex<Vec<Listener>>,
}

/// Subscribes to the net service's WifiStateCallback broadcast and
/// maintains a shared snapshot of the WiFi state.
pub struct WifiObserver {
    inner: Arc<Inner>,
    /// `None` for test-mode observers (no IPC worker).
    worker: Option<JoinHandle<()>>,
    /// CID we send `SHUTDOWN_OPCODE` to in order to wake the worker.
    /// `None` for test mode.
    shutdown_cid: Option<CID>,
    /// SID the worker receives on. `None` for test mode.
    worker_sid: Option<SID>,
    /// NetManager owns the net-side subscription. Drop triggers
    /// `wifi_state_unsubscribe`. `None` for test mode.
    netmgr: Option<NetManager>,
}

impl WifiObserver {
    /// Construct an observer that subscribes to the net service's
    /// WifiStateCallback broadcast.
    ///
    /// Initial `link_state` is seeded via a single `wlan_sync_state()`
    /// query because the broadcast only fires on state changes; without
    /// the seed, an observer constructed when WiFi is already up would
    /// see `LinkState::Unknown` until the next change. The query is a
    /// scalar IPC (no buffer allocation), so cheap.
    pub fn new() -> io::Result<Self> {
        let xns = xous_names::XousNames::new()
            .map_err(|e| io::Error::other(format!("XousNames::new: {e:?}")))?;

        // Seed initial state via a direct wlan_sync_state query. Best-
        // effort: on hosted mode (no EC) or transient COM error, fall
        // back to Unknown — the first broadcast will correct it.
        let initial_link = com::Com::new(&xns)
            .ok()
            .and_then(|com| com.wlan_sync_state().ok())
            .map(|(link, _dhcp)| link)
            .unwrap_or(LinkState::Unknown);

        let inner = Arc::new(Inner {
            state: RwLock::new(WifiState {
                link_state: initial_link,
                last_change: Instant::now(),
            }),
            waiter: (Mutex::new(()), Condvar::new()),
            listeners: Mutex::new(Vec::new()),
        });

        let sid = xous::create_server()
            .map_err(|e| io::Error::other(format!("create_server: {e:?}")))?;
        let cid = xous::connect(sid)
            .map_err(|e| io::Error::other(format!("connect to own sid: {e:?}")))?;

        let mut netmgr = NetManager::new();
        netmgr
            .wifi_state_subscribe(cid, UPDATE_OPCODE)
            .map_err(|e| io::Error::other(format!("wifi_state_subscribe: {e:?}")))?;

        let worker_inner = inner.clone();
        let worker = thread::Builder::new()
            .name("xsc-wifi-observer".into())
            .spawn(move || worker_loop(sid, worker_inner))
            .map_err(|e| io::Error::other(format!("thread spawn: {e}")))?;

        Ok(Self {
            inner,
            worker: Some(worker),
            shutdown_cid: Some(cid),
            worker_sid: Some(sid),
            netmgr: Some(netmgr),
        })
    }

    /// Construct a test-mode observer. No net subscription; no worker
    /// thread. State changes only via `inject_for_test`.
    pub fn for_test() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: RwLock::new(WifiState::unknown()),
                waiter: (Mutex::new(()), Condvar::new()),
                listeners: Mutex::new(Vec::new()),
            }),
            worker: None,
            shutdown_cid: None,
            worker_sid: None,
            netmgr: None,
        }
    }

    /// Snapshot of the current state.
    pub fn current(&self) -> WifiState {
        self.inner.state.read().unwrap().clone()
    }

    /// `true` iff the most recent broadcast reported `LinkState::Connected`.
    pub fn is_connected(&self) -> bool {
        self.inner.state.read().unwrap().is_connected()
    }

    /// Block until `link_state` becomes `Connected`, or the timeout
    /// elapses.
    ///
    /// Returns `Ok(())` immediately if already connected. Returns an
    /// `io::Error` of kind `TimedOut` if the timeout fires first.
    pub fn wait_for_connection(&self, timeout: Duration) -> io::Result<()> {
        if self.is_connected() {
            return Ok(());
        }
        let deadline = Instant::now() + timeout;
        let (lock, cvar) = &self.inner.waiter;
        let mut guard = lock.lock().unwrap();
        while !self.is_connected() {
            let remaining = match deadline.checked_duration_since(Instant::now()) {
                Some(d) if !d.is_zero() => d,
                _ => return Err(timed_out()),
            };
            let (g, result) = cvar.wait_timeout(guard, remaining).unwrap();
            guard = g;
            if result.timed_out() && !self.is_connected() {
                return Err(timed_out());
            }
        }
        Ok(())
    }

    /// Register a listener that fires on every state update. Runs on
    /// the observer's worker thread; keep it cheap and non-blocking.
    /// Listeners are not removable individually — they live until the
    /// observer is dropped.
    pub fn subscribe<F>(&self, f: F)
    where
        F: Fn(&WifiState) + Send + Sync + 'static,
    {
        self.inner.listeners.lock().unwrap().push(Arc::new(f));
    }

    /// Inject a synthetic state update. Used by unit tests and hosted-
    /// mode integration tests to drive transitions without a real net
    /// broadcast. Production code does not call this.
    pub fn inject_for_test(&self, link_state: LinkState) {
        let new_state = WifiState { link_state, last_change: Instant::now() };
        update_state(&self.inner, new_state);
    }
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "wait_for_connection timed out")
}

/// Apply a new state to the shared snapshot, notify cvar waiters, and
/// fan out to listeners. Rejects out-of-order updates so `last_change`
/// is monotonic.
fn update_state(inner: &Arc<Inner>, new_state: WifiState) {
    {
        let mut w = inner.state.write().unwrap();
        if new_state.last_change < w.last_change {
            return;
        }
        *w = new_state.clone();
    }
    {
        let _g = inner.waiter.0.lock().unwrap();
        inner.waiter.1.notify_all();
    }
    let listeners: Vec<Listener> = inner.listeners.lock().unwrap().clone();
    for l in listeners {
        l(&new_state);
    }
}

fn worker_loop(sid: SID, inner: Arc<Inner>) {
    loop {
        let msg = match xous::receive_message(sid) {
            Ok(m) => m,
            Err(e) => {
                log::error!("wifi_observer: receive_message error: {e:?}");
                break;
            }
        };
        let opcode = msg.body.id() as u32;
        if opcode == SHUTDOWN_OPCODE {
            break;
        }
        if opcode != UPDATE_OPCODE {
            log::warn!("wifi_observer: ignoring unknown opcode {opcode}");
            continue;
        }
        let mem_msg = match msg.body.memory_message() {
            Some(m) => m,
            None => {
                log::warn!("wifi_observer: update opcode without memory message");
                continue;
            }
        };
        let buffer = unsafe { Buffer::from_memory_message(mem_msg) };
        let ipc: WlanStatusIpc = match buffer.to_original() {
            Ok(v) => v,
            Err(e) => {
                log::warn!("wifi_observer: WlanStatusIpc decode failed: {e:?}");
                continue;
            }
        };
        let new_state = WifiState {
            link_state: LinkState::decode_u16(ipc.link_state),
            last_change: Instant::now(),
        };
        update_state(&inner, new_state);
    }
}

impl Drop for WifiObserver {
    fn drop(&mut self) {
        if let Some(cid) = self.shutdown_cid.take() {
            let _ = xous::send_message(
                cid,
                xous::Message::new_scalar(SHUTDOWN_OPCODE as usize, 0, 0, 0, 0),
            );
            unsafe {
                let _ = xous::disconnect(cid);
            }
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // Drop NetManager explicitly so unsubscribe runs before we
        // destroy our worker SID. NetManager::Drop sends Drop to the
        // forwarder thread, which then exits its own onetime SID.
        let _ = self.netmgr.take();
        if let Some(sid) = self.worker_sid.take() {
            let _ = xous::destroy_server(sid);
        }
    }
}

// ============================ tests ============================

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for<F: Fn() -> bool>(predicate: F, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        predicate()
    }

    #[test]
    fn current_returns_most_recent_state() {
        let obs = WifiObserver::for_test();
        assert_eq!(obs.current().link_state, LinkState::Unknown);

        obs.inject_for_test(LinkState::Connecting);
        assert_eq!(obs.current().link_state, LinkState::Connecting);

        obs.inject_for_test(LinkState::Connected);
        assert_eq!(obs.current().link_state, LinkState::Connected);

        obs.inject_for_test(LinkState::Disconnected);
        assert_eq!(obs.current().link_state, LinkState::Disconnected);
    }

    #[test]
    fn last_change_is_monotonic() {
        let obs = WifiObserver::for_test();
        let t0 = obs.current().last_change;
        thread::sleep(Duration::from_millis(2));
        obs.inject_for_test(LinkState::Connecting);
        let t1 = obs.current().last_change;
        thread::sleep(Duration::from_millis(2));
        obs.inject_for_test(LinkState::Connected);
        let t2 = obs.current().last_change;
        assert!(t1 > t0, "t1 ({:?}) should be > t0 ({:?})", t1, t0);
        assert!(t2 > t1, "t2 ({:?}) should be > t1 ({:?})", t2, t1);
    }

    #[test]
    fn is_connected_matches_link_state_predicate() {
        let obs = WifiObserver::for_test();
        for ls in [
            LinkState::Unknown,
            LinkState::ResetHold,
            LinkState::Uninitialized,
            LinkState::Initializing,
            LinkState::Disconnected,
            LinkState::Connecting,
            LinkState::WFXError,
        ] {
            obs.inject_for_test(ls);
            assert!(!obs.is_connected(), "{:?} should not be connected", ls);
        }
        obs.inject_for_test(LinkState::Connected);
        assert!(obs.is_connected());
    }

    #[test]
    fn wait_for_connection_returns_immediately_when_already_connected() {
        let obs = WifiObserver::for_test();
        obs.inject_for_test(LinkState::Connected);
        let start = Instant::now();
        obs.wait_for_connection(Duration::from_secs(1)).expect("should be Ok");
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn wait_for_connection_returns_ok_when_event_fires() {
        let obs = Arc::new(WifiObserver::for_test());
        let obs_for_thread = obs.clone();
        let waker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            obs_for_thread.inject_for_test(LinkState::Connected);
        });
        obs.wait_for_connection(Duration::from_secs(2))
            .expect("wait should succeed once Connected fires");
        waker.join().unwrap();
    }

    #[test]
    fn wait_for_connection_times_out_when_no_event_fires() {
        let obs = WifiObserver::for_test();
        let start = Instant::now();
        let err = obs
            .wait_for_connection(Duration::from_millis(75))
            .expect_err("should time out");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() >= Duration::from_millis(75));
    }

    #[test]
    fn wait_for_connection_keeps_waiting_on_non_connected_events() {
        let obs = Arc::new(WifiObserver::for_test());
        let obs_for_thread = obs.clone();
        let waker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            obs_for_thread.inject_for_test(LinkState::Connecting);
            thread::sleep(Duration::from_millis(20));
            obs_for_thread.inject_for_test(LinkState::Connected);
        });
        obs.wait_for_connection(Duration::from_secs(2)).expect("should succeed");
        waker.join().unwrap();
    }

    #[test]
    fn subscribe_listener_fires_on_each_update() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let obs = WifiObserver::for_test();
        let count = Arc::new(AtomicUsize::new(0));
        let count_for_listener = count.clone();
        obs.subscribe(move |_state| {
            count_for_listener.fetch_add(1, Ordering::SeqCst);
        });
        obs.inject_for_test(LinkState::Connecting);
        obs.inject_for_test(LinkState::Connected);
        obs.inject_for_test(LinkState::Disconnected);
        assert!(wait_for(
            || count.load(Ordering::SeqCst) == 3,
            Duration::from_millis(200),
        ));
    }
}
