//! Provisioning-only WebSocket worker for sigchat device linking.
//!
//! This module owns the WebSocket connection to Signal's `/v1/websocket/provisioning/`
//! endpoint from QR-display through ProvisionEnvelope delivery. It exists to keep the
//! WS alive while the QR modal blocks the main thread: the worker runs in a dedicated
//! thread, sends 25s application-layer Pings, and surfaces the Binary envelope back to
//! the caller via a 2-opcode Xous IPC interface (`WaitAndTakeBinary`, `Cancel`).
//!
//! Lifecycle is strictly one-shot. The worker is spawned once per `Manager::link()`
//! call and destroyed when linking completes (success or failure). Messaging and
//! group chat require a separate long-running worker against Signal's main
//! `/v1/websocket` endpoint with its own opcode enum, lifecycle, and integration
//! with `libs/chat`'s event model — do not generalize this module to cover that case.

use std::io;
use std::thread;
use std::time::Duration;

use num_traits::FromPrimitive;
use rkyv::{Archive, Deserialize, Serialize};
use ticktimer_server::Ticktimer;
use tungstenite::Message;
use xous::{CID, SID};
use xous_ipc::Buffer;

use crate::manager::signal_ws::SignalWS;

/// Max size of a single ProvisionEnvelope frame accepted from the server.
/// 64 KiB is 16 pages — Signal's typical envelope is ~4KB; this gives 16x
/// headroom for protocol evolution. Larger frames are rejected with an
/// explicit error and the received size is logged for diagnostics.
pub(crate) const WS_PROVISION_MAX: usize = 64 * 1024;

/// Application-layer keepalive cadence. Signal's documented server-side idle
/// timeout is ~60s; 25s leaves room for two Pings before the server drops us.
/// Hardcoded by design — runtime configurability was rejected (no use case).
const KEEPALIVE_MS: u64 = 25_000;

/// Per-iteration read timeout on the underlying TCP stream, set via
/// `SignalWS::set_read_timeout`. Short enough that the worker cycles back
/// to `try_receive_message` promptly; long enough that we do not spin.
const READ_TIMEOUT_MS: u64 = 500;

#[derive(Debug, num_derive::FromPrimitive, num_derive::ToPrimitive)]
pub(crate) enum WsServerOp {
    /// lend_mut memory message carrying a `WsResultBuf`. The main thread
    /// blocks inside `.lend_mut()` until the worker populates the buffer
    /// with a terminal result (Binary, Closed, TimedOut, Cancelled, Error)
    /// and returns the page.
    WaitAndTakeBinary = 0,
    /// blocking_scalar — main thread calls this both to abort early and as
    /// the clean-shutdown handshake on success. Worker replies with
    /// `return_scalar(sender, 1)`, drains any parked waiter with
    /// `Cancelled`, and destroys its SID. Mirrors `WifiStateCallback::Drop`.
    Cancel = 1,
}

// Status codes serialized in WsResultBuf.status.
pub(crate) const STATUS_BINARY: u8 = 0;
pub(crate) const STATUS_CLOSED: u8 = 1;
pub(crate) const STATUS_TIMED_OUT: u8 = 2;
pub(crate) const STATUS_CANCELLED: u8 = 3;
pub(crate) const STATUS_ERROR: u8 = 4;

/// Fixed-size payload for the `WaitAndTakeBinary` lend_mut. `bytes[..len]`
/// is the ProvisionEnvelope when `status == STATUS_BINARY`; otherwise `len`
/// is 0 and `status` is one of the non-Binary codes above.
#[derive(Archive, Serialize, Deserialize, Debug)]
pub(crate) struct WsResultBuf {
    pub status: u8,
    pub len: u32,
    pub bytes: [u8; WS_PROVISION_MAX],
}

impl WsResultBuf {
    pub fn empty() -> Self {
        Self { status: 255, len: 0, bytes: [0u8; WS_PROVISION_MAX] }
    }
}

/// Terminal result observed by the caller after `wait_and_take_binary`.
#[derive(Debug)]
pub enum WsResult {
    Binary(Vec<u8>),
    Closed,
    TimedOut,
    Cancelled,
    Error(io::Error),
}

pub struct SignalWsServer {
    cid: CID,
}

impl SignalWsServer {
    /// Spawn a worker thread that takes ownership of `ws` and drives it
    /// (keepalive + read) until a terminal result is available. `deadline_secs`
    /// bounds the total session lifetime (measured from spawn time); when it
    /// elapses without a Binary, the worker produces `TimedOut`.
    pub fn spawn(ws: SignalWS, deadline_secs: u64) -> io::Result<Self> {
        // xous::create_server failure is a true unrecoverable environmental
        // error — the only panic site authorised by the design.
        let sid = xous::create_server().expect("ws_server: create_server failed");
        let cid = xous::connect(sid).expect("ws_server: connect to own SID failed");
        let builder = thread::Builder::new().name("sigchat-ws-server".into());
        builder
            .spawn(move || worker_loop(sid, ws, deadline_secs))
            .map_err(|e| io::Error::other(format!("thread spawn: {e}")))?;
        Ok(Self { cid })
    }

    /// Block until the worker has a terminal result, then retrieve it. The
    /// buffer crosses the Xous IPC boundary via a 64 KiB `WsResultBuf` page;
    /// the caller owns a standard `Vec<u8>` on return.
    pub fn wait_and_take_binary(&self) -> WsResult {
        let result_buf = WsResultBuf::empty();
        let mut ipc = match Buffer::into_buf(result_buf) {
            Ok(b) => b,
            Err(e) => {
                return WsResult::Error(io::Error::other(format!("ipc into_buf: {e:?}")));
            }
        };
        if let Err(e) = ipc.lend_mut(self.cid, WsServerOp::WaitAndTakeBinary as u32) {
            return WsResult::Error(io::Error::other(format!("ipc lend_mut: {e:?}")));
        }
        let result = match ipc.to_original::<WsResultBuf, _>() {
            Ok(r) => r,
            Err(e) => {
                return WsResult::Error(io::Error::other(format!("ipc to_original: {e:?}")));
            }
        };
        match result.status {
            STATUS_BINARY => {
                let len = result.len as usize;
                if len > WS_PROVISION_MAX {
                    return WsResult::Error(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "payload len exceeds WS_PROVISION_MAX",
                    ));
                }
                WsResult::Binary(result.bytes[..len].to_vec())
            }
            STATUS_CLOSED => WsResult::Closed,
            STATUS_TIMED_OUT => WsResult::TimedOut,
            STATUS_CANCELLED => WsResult::Cancelled,
            STATUS_ERROR => WsResult::Error(io::Error::other("worker error")),
            other => WsResult::Error(io::Error::other(format!("unknown WsResultBuf.status {other}"))),
        }
    }

    /// Synchronous shutdown handshake. Also the ONLY way to cleanly destroy
    /// the worker's SID, so every `link()` exit path must call this — on
    /// success as a drain, on error as an abort. Consumes `self` to prevent
    /// stale CID reuse after disconnect.
    pub fn cancel(self) -> io::Result<()> {
        let res = xous::send_message(
            self.cid,
            xous::Message::new_blocking_scalar(WsServerOp::Cancel as usize, 0, 0, 0, 0),
        );
        // We disconnect our CID regardless of the reply result — the worker
        // always destroys its SID once it reaches the Cancel branch.
        unsafe {
            let _ = xous::disconnect(self.cid);
        }
        res.map(|_| ()).map_err(|e| io::Error::other(format!("cancel: {e:?}")))
    }
}

// ==================== worker internals ====================

/// Internal pending result. Maps 1:1 to the STATUS_* codes written into the
/// lend_mut buffer by `fill_buf`.
enum PendingResult {
    Binary(Vec<u8>),
    Closed,
    TimedOut,
    Cancelled,
    Error,
}

fn worker_loop(sid: SID, mut ws: SignalWS, deadline_secs: u64) {
    // INVARIANT: this function — and every branch it dispatches to — MUST NOT
    // panic once execution enters the message-handler loop. A panic leaks the
    // live SID and hangs any caller blocked on `WaitAndTakeBinary.lend_mut`
    // or `Cancel.blocking_scalar`. All fallible operations below translate
    // failures into `PendingResult::Error` or a `log::warn!` + continue, never
    // into `panic!`/`unwrap()`/`expect()`. The only permitted panic site is
    // in `SignalWsServer::spawn` for `xous::create_server` failure, which is a
    // true unrecoverable environmental error detected before any worker thread
    // is visible to the caller.

    // Install the per-read timeout so ws.read() returns WouldBlock/TimedOut
    // in ~500ms, letting the worker service IPC during idle periods.
    if let Err(e) = ws.set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS))) {
        log::warn!("ws_server: set_read_timeout failed: {e}; loop may not cycle during idle");
    }

    let tt = match Ticktimer::new() {
        Ok(t) => t,
        Err(e) => {
            // Worker cannot function without timing. Bail — the main thread's
            // lend_mut will surface an IPC error when the worker never replies.
            // SID is intentionally not destroyed here: xous::destroy_server
            // needs the SID value, which we do hold, but the main-thread error
            // path already handles the "worker never responded" case.
            log::error!("ws_server: Ticktimer::new failed: {e:?}; aborting worker without destroying SID");
            return;
        }
    };

    let deadline_ms = tt.elapsed_ms() + deadline_secs.saturating_mul(1000);
    let mut last_send_at = tt.elapsed_ms();
    let mut pending: Option<PendingResult> = None;
    let mut waiter: Option<xous::MessageEnvelope> = None;

    loop {
        // (1) Non-blocking IPC service.
        match xous::try_receive_message(sid) {
            Ok(Some(msg)) => {
                let opcode: Option<WsServerOp> = FromPrimitive::from_usize(msg.body.id());
                match opcode {
                    Some(WsServerOp::WaitAndTakeBinary) => {
                        if let Some(result) = pending.take() {
                            fill_buf(msg, result);
                        } else {
                            waiter = Some(msg);
                        }
                    }
                    Some(WsServerOp::Cancel) => {
                        if let Some(w) = waiter.take() {
                            fill_buf(w, PendingResult::Cancelled);
                        }
                        if let Err(e) = xous::return_scalar(msg.sender, 1) {
                            log::warn!("ws_server: return_scalar failed on Cancel: {e:?}");
                        }
                        break;
                    }
                    None => {
                        // Xous IPC is a trust boundary; any process with this
                        // SID can send any opcode. Log and continue — panicking
                        // would give a hostile sender a reliable DoS on the
                        // caller's blocking lend_mut.
                        log::warn!("ws_server: unknown opcode id={} (ignored)", msg.body.id());
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                log::error!("ws_server: try_receive_message error: {e:?}; exiting loop");
                break;
            }
        }

        // (2) Deadline.
        if pending.is_none() && tt.elapsed_ms() >= deadline_ms {
            log::warn!("ws_server: deadline reached without ProvisionEnvelope");
            pending = Some(PendingResult::TimedOut);
        }

        // (3) Application-layer keepalive.
        if pending.is_none() && tt.elapsed_ms().saturating_sub(last_send_at) >= KEEPALIVE_MS {
            match ws.send(Message::Ping(Vec::new())) {
                Ok(()) => {
                    last_send_at = tt.elapsed_ms();
                    log::debug!("ws_server: sent app-layer Ping");
                }
                Err(e) => {
                    log::warn!("ws_server: keepalive Ping send failed: {e}");
                    pending = Some(PendingResult::Error);
                }
            }
        }

        // (4) Drive ws.read with the underlying 500ms timeout.
        if pending.is_none() {
            match ws.read() {
                Ok(Message::Binary(b)) => {
                    if b.len() > WS_PROVISION_MAX {
                        log::error!(
                            "ws_server: ProvisionEnvelope size {} exceeds {}",
                            b.len(),
                            WS_PROVISION_MAX
                        );
                        pending = Some(PendingResult::Error);
                    } else {
                        log::info!("ws_server: received Binary ({} bytes)", b.len());
                        pending = Some(PendingResult::Binary(b));
                    }
                }
                Ok(Message::Ping(_)) => {
                    log::debug!("ws_server: got server Ping (tungstenite auto-Pong queued)");
                }
                Ok(Message::Pong(_)) => {
                    log::debug!("ws_server: got server Pong");
                }
                Ok(Message::Text(t)) => {
                    log::debug!("ws_server: got Text ({} chars); ignored", t.len());
                }
                Ok(Message::Frame(_)) => {
                    log::debug!("ws_server: got raw Frame; ignored");
                }
                Ok(Message::Close(c)) => {
                    log::info!("ws_server: got Close from server: {c:?}");
                    pending = Some(PendingResult::Closed);
                }
                Err(e) => {
                    if is_timeout(&e) {
                        // Expected: 500ms cycle timeout with no data.
                    } else {
                        log::warn!("ws_server: ws read error: {e}");
                        pending = Some(PendingResult::Error);
                    }
                }
            }
        }

        // (5) Deliver a pending result if main is waiting.
        if pending.is_some() && waiter.is_some() {
            if let (Some(result), Some(msg)) = (pending.take(), waiter.take()) {
                fill_buf(msg, result);
            }
            // Do not break — stay alive until Cancel arrives, so main's
            // drain handshake always has something to reply to.
        }
    }

    log::info!("ws_server: worker loop exited; closing ws and destroying SID");
    ws.close();
    if let Err(e) = xous::destroy_server(sid) {
        log::warn!("ws_server: destroy_server failed: {e:?}");
    }
}

/// Writes `result` into the lend_mut page carried by `msg` and drops `msg`,
/// returning the page to the caller. Any serialization failure is logged and
/// the page returns unmodified (caller will see status=255 → Error).
fn fill_buf(mut msg: xous::MessageEnvelope, result: PendingResult) {
    let memory = match msg.body.memory_message_mut() {
        Some(m) => m,
        None => {
            log::warn!("ws_server: WaitAndTakeBinary arrived with non-memory body; dropping");
            return;
        }
    };
    let mut buffer = unsafe { Buffer::from_memory_message_mut(memory) };
    let mut payload = match buffer.to_original::<WsResultBuf, _>() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("ws_server: deserialize WsResultBuf failed: {e:?}");
            return;
        }
    };
    match result {
        PendingResult::Binary(b) => {
            payload.status = STATUS_BINARY;
            payload.len = b.len() as u32;
            payload.bytes[..b.len()].copy_from_slice(&b);
        }
        PendingResult::Closed => payload.status = STATUS_CLOSED,
        PendingResult::TimedOut => payload.status = STATUS_TIMED_OUT,
        PendingResult::Cancelled => payload.status = STATUS_CANCELLED,
        PendingResult::Error => payload.status = STATUS_ERROR,
    }
    if let Err(e) = buffer.replace(payload) {
        log::warn!("ws_server: re-serialize WsResultBuf failed: {e:?}");
    }
    // `buffer` borrows into `msg.body`; both drop at function end. Page
    // returns to sender automatically when `msg` is dropped.
}

fn is_timeout(e: &tungstenite::Error) -> bool {
    if let tungstenite::Error::Io(io_err) = e {
        matches!(io_err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test coverage below is limited on purpose:
    // - Xous IPC (create_server, receive_message, return_scalar) cannot be
    //   exercised without a running hosted Xous environment, so the
    //   opcode-handling branches of worker_loop are NOT covered here. The
    //   end-to-end scan is the final test for that layer.
    // - rustls / tungstenite cannot be spun up against a real TLS endpoint
    //   in-process. The critical rustls-xous timeout-propagation assertion
    //   is therefore validated by a raw-TcpStream test below plus the
    //   eventual end-to-end scan.

    #[test]
    fn is_timeout_matches_wouldblock_and_timedout() {
        let wb = tungstenite::Error::Io(io::Error::from(io::ErrorKind::WouldBlock));
        let to = tungstenite::Error::Io(io::Error::from(io::ErrorKind::TimedOut));
        assert!(is_timeout(&wb));
        assert!(is_timeout(&to));
    }

    #[test]
    fn is_timeout_rejects_non_timeout_errors() {
        let closed = tungstenite::Error::ConnectionClosed;
        let already = tungstenite::Error::AlreadyClosed;
        let other_io =
            tungstenite::Error::Io(io::Error::from(io::ErrorKind::ConnectionReset));
        assert!(!is_timeout(&closed));
        assert!(!is_timeout(&already));
        assert!(!is_timeout(&other_io));
    }

    #[test]
    fn ws_result_buf_empty_is_sentinel() {
        let b = WsResultBuf::empty();
        assert_eq!(b.status, 255);
        assert_eq!(b.len, 0);
        assert!(b.bytes.iter().all(|x| *x == 0));
    }

    /// Raw TcpStream timeout behavior — the contract our worker's 500ms loop
    /// relies on. This does NOT cover rustls-xous propagation (see module
    /// header); it pins the underlying std behavior so a regression here
    /// would be obvious independent of the TLS layer.
    #[test]
    fn tcpstream_read_timeout_fires_within_budget() {
        use std::io::Read as _;
        use std::net::{TcpListener, TcpStream};
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        // Accept on a helper thread, but never write — forces the client
        // into a pure-read-timeout scenario.
        let _accept = std::thread::spawn(move || {
            let (_sock, _) = listener.accept().expect("accept");
            // Hold the socket open until the test drops its TcpStream.
            std::thread::sleep(Duration::from_secs(2));
        });

        let mut client = TcpStream::connect(addr).expect("connect");
        client.set_read_timeout(Some(Duration::from_millis(500))).expect("set_read_timeout");

        let start = Instant::now();
        let mut buf = [0u8; 16];
        let result = client.read(&mut buf);
        let elapsed = start.elapsed();

        assert!(result.is_err(), "expected read to error with timeout");
        let err = result.unwrap_err();
        assert!(
            matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut),
            "unexpected error kind: {:?}",
            err.kind()
        );
        // 500ms nominal + generous scheduler slop.
        assert!(
            elapsed >= Duration::from_millis(450),
            "read returned suspiciously fast ({:?}) — timeout probably not applied",
            elapsed
        );
        assert!(
            elapsed <= Duration::from_millis(1500),
            "read took far longer than 500ms budget ({:?}) — timeout not honored",
            elapsed
        );
    }

    /// `WsResultBuf` must round-trip through the xous_ipc Buffer layer — i.e.
    /// rkyv serialize → deserialize preserves status/len/first N bytes.
    /// Proves the IPC payload layer carries the data as expected.
    ///
    /// Ignored by default: `xous_ipc::Buffer::into_buf` asserts inside the
    /// hosted-mode PID init block, which only runs under the xous runtime.
    /// Run manually with `cargo test ... -- --ignored` in a hosted build.
    #[test]
    #[ignore = "requires xous hosted-mode runtime for Buffer::into_buf"]
    fn ws_result_buf_roundtrips_via_xous_ipc() {
        let mut original = WsResultBuf::empty();
        original.status = STATUS_BINARY;
        original.len = 5;
        original.bytes[..5].copy_from_slice(&[1, 2, 3, 4, 5]);

        let ipc = xous_ipc::Buffer::into_buf(original).expect("into_buf");
        let decoded = ipc.to_original::<WsResultBuf, _>().expect("to_original");

        assert_eq!(decoded.status, STATUS_BINARY);
        assert_eq!(decoded.len, 5);
        assert_eq!(&decoded.bytes[..5], &[1, 2, 3, 4, 5]);
        assert!(decoded.bytes[5..].iter().all(|b| *b == 0));
    }
}
