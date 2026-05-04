//! Integration test: simulate WiFi flap recovery without going near
//! real Xous IPC, the WS server, or any HTTP layer.
//!
//! Composition under test:
//! - `WifiObserver::for_test()` as the source of synthetic state events
//! - `CancellationToken` + `FlapWatcher` wiring a cancellation signal
//! - A mock long-running operation that polls cancellation and exits
//!   promptly when the flag flips
//! - The retry-once shape from `Manager::link()`: wait for reconnect,
//!   build a new token, run the operation again
//!
//! What this proves: the abstract flap-recovery composition is
//! correct. The Xous-specific pieces (real net broadcast, real WS
//! worker, real HTTP path) need separate validation on hardware and
//! are documented in the session report.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use com_rs::LinkState;
use xous_signal_client::manager::cancellation::{
    CancellationHandle, CancellationToken, FlapWatcher,
};
use xous_signal_client::manager::wifi_observer::WifiObserver;

/// Mock long-running operation that polls a cancellation handle every
/// 5ms. Returns `Ok(value)` if `work_steps` ticks complete, or
/// `Err("cancelled")` if the handle flips before completion. Mirrors
/// the contract a real long-blocking call (TLS handshake, WS read)
/// would have to honour.
fn mock_long_op(
    handle: &CancellationHandle,
    work_steps: usize,
    value: u32,
) -> Result<u32, &'static str> {
    for _ in 0..work_steps {
        if handle.is_cancelled() {
            return Err("cancelled");
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(value)
}

/// One end-to-end attempt. Sets up the FlapWatcher, runs the op,
/// returns the result and the (per-attempt) token so the caller can
/// inspect why it failed.
fn one_attempt(
    observer: &WifiObserver,
    work_steps: usize,
    value: u32,
) -> (Result<u32, &'static str>, CancellationToken) {
    let token = CancellationToken::new();
    let _watcher = FlapWatcher::for_handle(observer, token.handle());
    let result = mock_long_op(&token.handle(), work_steps, value);
    (result, token)
}

#[test]
fn cancellation_propagates_to_long_op() {
    let observer = WifiObserver::for_test();
    observer.inject_for_test(LinkState::Connected);

    let token = CancellationToken::new();
    let _watcher = FlapWatcher::for_handle(&observer, token.handle());

    // Spawn the op on a worker so we can flap from the main thread.
    let handle_for_op = token.handle();
    let op = thread::spawn(move || mock_long_op(&handle_for_op, 100, 42));

    // Give the op time to enter its loop, then simulate a flap.
    thread::sleep(Duration::from_millis(20));
    observer.inject_for_test(LinkState::Disconnected);

    let result = op.join().expect("op thread panicked");
    assert_eq!(result, Err("cancelled"));
    assert!(token.is_cancelled());
}

#[test]
fn no_flap_lets_op_complete_normally() {
    let observer = WifiObserver::for_test();
    observer.inject_for_test(LinkState::Connected);

    let token = CancellationToken::new();
    let _watcher = FlapWatcher::for_handle(&observer, token.handle());

    // Short op; should finish before any (absent) flap.
    let result = mock_long_op(&token.handle(), 5, 7);
    assert_eq!(result, Ok(7));
    assert!(!token.is_cancelled());
}

#[test]
fn retry_after_simulated_flap_succeeds() {
    let observer = WifiObserver::for_test();
    observer.inject_for_test(LinkState::Connected);

    // Attempt #1: starts, flap interrupts it.
    let attempt1_done = Arc::new(AtomicBool::new(false));
    let attempt1_done_for_thread = attempt1_done.clone();

    let observer_arc = Arc::new(observer);
    let observer_for_thread = observer_arc.clone();

    // We need to pass the token across threads. Spawn a thread that
    // sets up token + watcher + runs op; outer test triggers flap.
    let result1_handle = thread::spawn(move || {
        let token = CancellationToken::new();
        let _watcher =
            FlapWatcher::for_handle(observer_for_thread.as_ref(), token.handle());
        let r = mock_long_op(&token.handle(), 100, 1);
        attempt1_done_for_thread.store(true, Ordering::SeqCst);
        (r, token.is_cancelled())
    });

    thread::sleep(Duration::from_millis(20));
    observer_arc.inject_for_test(LinkState::Disconnected);

    let (r1, was_cancelled) = result1_handle.join().expect("attempt 1 thread panicked");
    assert_eq!(r1, Err("cancelled"));
    assert!(was_cancelled);
    assert!(attempt1_done.load(Ordering::SeqCst));

    // Caller sees the cancel; waits for reconnect.
    let observer_for_reconnect = observer_arc.clone();
    let reconnect_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(30));
        observer_for_reconnect.inject_for_test(LinkState::Connected);
    });
    observer_arc
        .wait_for_connection(Duration::from_secs(2))
        .expect("should reconnect");
    reconnect_thread.join().unwrap();

    // Attempt #2: fresh token + watcher. No flap injected; should
    // complete.
    let token2 = CancellationToken::new();
    let _w2 = FlapWatcher::for_handle(observer_arc.as_ref(), token2.handle());
    let r2 = mock_long_op(&token2.handle(), 5, 2);
    assert_eq!(r2, Ok(2));
    assert!(!token2.is_cancelled());
}

#[test]
fn multiple_flaps_only_cancel_once_per_token() {
    // Token is one-way: once cancelled, stays cancelled. Multiple
    // flap events don't double-cancel anything.
    let observer = WifiObserver::for_test();
    observer.inject_for_test(LinkState::Connected);

    let token = CancellationToken::new();
    let cancel_count = Arc::new(AtomicUsize::new(0));
    let cancel_count_for_listener = cancel_count.clone();
    let token_handle = token.handle();
    let _watcher = FlapWatcher::new(&observer, move || {
        cancel_count_for_listener.fetch_add(1, Ordering::SeqCst);
        token_handle.cancel();
    });

    // First flap.
    observer.inject_for_test(LinkState::Disconnected);
    thread::sleep(Duration::from_millis(20));
    assert!(token.is_cancelled());
    assert_eq!(cancel_count.load(Ordering::SeqCst), 1);

    // Reconnect + flap again — listener fires again because the
    // FlapWatcher detects a new edge. But the token is already
    // cancelled (one-way), so the operation's outer state stays
    // canceled. The cancel_count is purely for observability —
    // production cancel(handle) is idempotent.
    observer.inject_for_test(LinkState::Connected);
    observer.inject_for_test(LinkState::Disconnected);
    thread::sleep(Duration::from_millis(20));
    assert!(token.is_cancelled());
    assert_eq!(cancel_count.load(Ordering::SeqCst), 2);
}

#[test]
fn cancellation_latency_is_under_100ms() {
    // The operation should react to a flap quickly enough that the
    // human-perceptible "stuck" time is short. Bound: under 100ms
    // including the 5ms work-step granularity. Soaks generously to
    // smoke out scheduler edge cases.
    let observer = WifiObserver::for_test();
    observer.inject_for_test(LinkState::Connected);

    let token = CancellationToken::new();
    let _watcher = FlapWatcher::for_handle(&observer, token.handle());

    let handle_for_op = token.handle();
    let op = thread::spawn(move || mock_long_op(&handle_for_op, 1_000_000, 0));

    thread::sleep(Duration::from_millis(10));
    let flap_at = Instant::now();
    observer.inject_for_test(LinkState::Disconnected);

    let result = op.join().expect("op panicked");
    let elapsed = flap_at.elapsed();
    assert_eq!(result, Err("cancelled"));
    assert!(
        elapsed < Duration::from_millis(100),
        "cancellation latency {:?} exceeded 100ms budget",
        elapsed
    );
}

#[test]
fn no_corruption_after_cancel_during_simulated_pddb_writes() {
    // Models the "cancel during PDDB write" concern from the spec.
    // We don't exercise the real PDDB here (out of scope for hosted
    // unit tests), but we verify that the cancellation cooperates
    // with a multi-step "transaction" so the caller can observe a
    // clean partial-completion boundary instead of a half-written
    // mess.
    //
    // Pattern: each step is atomic (writes a single counter
    // increment). On cancel, the loop exits between steps. The
    // counter reflects exactly the number of completed steps — no
    // torn write.
    let observer = WifiObserver::for_test();
    observer.inject_for_test(LinkState::Connected);

    let token = CancellationToken::new();
    let _watcher = FlapWatcher::for_handle(&observer, token.handle());

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_for_op = counter.clone();
    let handle_for_op = token.handle();

    let op = thread::spawn(move || {
        for i in 0..100 {
            if handle_for_op.is_cancelled() {
                return Err(("cancelled", i));
            }
            counter_for_op.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    });

    thread::sleep(Duration::from_millis(30));
    observer.inject_for_test(LinkState::Disconnected);

    let result = op.join().expect("op panicked");
    let final_count = counter.load(Ordering::SeqCst);
    let (_, attempted_at_cancel) = result.expect_err("should have cancelled");
    // Counter == steps completed; attempted_at_cancel is the index
    // of the loop iteration that observed cancellation. They must
    // be equal — otherwise we'd have either a torn step (counter >
    // attempted) or an undercount (counter < attempted, impossible
    // given fetch-then-cancel-check ordering).
    assert_eq!(final_count, attempted_at_cancel);
    // Sanity: at least a few steps actually ran before cancel.
    assert!(final_count > 0, "no steps completed before cancel");
    assert!(final_count < 100, "all steps somehow completed despite cancel");
}
