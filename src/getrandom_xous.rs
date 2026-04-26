// Xous custom backend for getrandom 0.3.x.
//
// Selected at compile time by --cfg getrandom_backend="custom" in
// .cargo/config.toml for the riscv32imac-unknown-xous-elf target only.
//
// Calls the Xous TRNG service via blocking-scalar messages, matching the
// small-buffer path in imports/getrandom-02/src/xous.rs (Task 3). The
// large lend_mut path (>=64 bytes) is omitted: it requires a second unsafe
// block (u8-to-u32 slice reinterpret) and getrandom 0.3.x callers (libsignal
// key material, nonces) request small amounts. The scalar path is correct
// for all sizes and keeps this file to exactly one unsafe block.

#![cfg(target_os = "xous")]

use core::sync::atomic::{AtomicU32, Ordering};
use getrandom::Error;

static TRNG_CONN: AtomicU32 = AtomicU32::new(0);

fn ensure_trng_conn() {
    if TRNG_CONN.load(Ordering::SeqCst) == 0 {
        let xns = xous_names::XousNames::new().unwrap();
        TRNG_CONN.store(
            xns.request_connection_blocking("_TRNG manager_")
                .expect("getrandom: can't connect to TRNG server"),
            Ordering::SeqCst,
        );
    }
}

fn next_u32() -> u32 {
    let response = xous::send_message(
        TRNG_CONN.load(Ordering::SeqCst),
        xous::Message::new_blocking_scalar(0 /* GetTrng */, 1, 0, 0, 0),
    )
    .expect("getrandom: TRNG scalar IPC failed");
    if let xous::Result::Scalar2(trng, _) = response {
        trng as u32
    } else {
        panic!("getrandom: unexpected TRNG response: {:#?}", response);
    }
}

fn next_u64() -> u64 {
    let response = xous::send_message(
        TRNG_CONN.load(Ordering::SeqCst),
        xous::Message::new_blocking_scalar(0 /* GetTrng */, 2, 0, 0, 0),
    )
    .expect("getrandom: TRNG scalar IPC failed");
    if let xous::Result::Scalar2(lo, hi) = response {
        lo as u64 | ((hi as u64) << 32)
    } else {
        panic!("getrandom: unexpected TRNG response: {:#?}", response);
    }
}

/// Fill `buf` with Xous TRNG entropy. No fallback; error is fatal (panic).
fn fill_from_trng(buf: &mut [u8]) {
    let mut left = buf;
    while left.len() >= 8 {
        let (chunk, rest) = left.split_at_mut(8);
        left = rest;
        chunk.copy_from_slice(&next_u64().to_ne_bytes());
    }
    let n = left.len();
    if n > 4 {
        left.copy_from_slice(&next_u64().to_ne_bytes()[..n]);
    } else if n > 0 {
        left.copy_from_slice(&next_u32().to_ne_bytes()[..n]);
    }
}

/// getrandom 0.3.x custom backend symbol.
///
/// Signature mandated by getrandom 0.3.x `src/backends/custom.rs`:
///   `fn __getrandom_v03_custom(dest: *mut u8, len: usize) -> Result<(), Error>`
///
/// # Safety
/// Caller (the `getrandom` crate) guarantees `dest` is valid for `len` bytes
/// of writes and the region is not aliased for the duration of the call.
#[no_mangle]
pub unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), Error> {
    if len == 0 {
        return Ok(());
    }
    ensure_trng_conn();
    // SAFETY: contract above. This is the sole unsafe block in this file.
    let buf: &mut [u8] = unsafe { core::slice::from_raw_parts_mut(dest, len) };
    fill_from_trng(buf);
    Ok(())
}
