# Squeezing sigchat into 1.5 MiB on Precursor

**The path to 1.5 MiB exists, but only by replacing libsignal's crypto providers and refusing to bundle a second TLS stack.** A defensible Proposal A keeps PQXDH by swapping `libcrux-ml-kem` for RustCrypto's `ml-kem`, routing the WebSocket through Xous's existing TLS service instead of linking another `rustls`+`ring-xous` instance, and applying the embedded-Rust binary-size discipline documented by the Tock/Hubris communities. Proposal B (drop PQ) frees roughly 0.5–0.8 MiB more but the resulting client cannot speak to current Signal servers, so it is only useful as a fallback architecture to be enabled if-and-when Signal exposes an opt-out, or for a non-Signal relay. The dominant fact is that sigchat's current 3.9 MB on x86_64 is mostly *code copied into RAM*, and Xous's own apps/README explicitly budgets ≤4 MiB for an app — 1.5 MiB is below that guidance and forces choices that go beyond switches: it forces a single-thread async architecture, a single TLS instance system-wide, and a curated crypto provider set that is shared across the entire Xous SBOM.

The rest of this report grounds these claims in concrete sizes (libcrux ~493 KB → ml-kem ~120 KB; ring-xous ~488 KB → shared with the kernel TLS service; defmt-style logging 9× smaller than `core::fmt`), then lays out an ordered, gated implementation sequence.

## The constraint that actually matters

Precursor has 16 MiB total SRAM, of which Xous OS itself consumes about 8 MiB (kernel + ~10 always-resident servers: ticktimer, log, names, gam, graphics, status, net, pddb, trng, llio, com, keyboard). The xous-core `apps/README.md` advises apps stay under 4 MiB because **code is copied from flash to RAM**, so the binary is the RSS. The 1.5 MiB target in this brief is below that guidance and below what `libs/chat` plus a libsignal-protocol subset would naturally land at. Two structural facts shape every option below:

First, **Xous's `tls` is a library, not a server.** Per xous-core issue #507 and bunnie's "Fully Oxidizing ring" blog (2022), TLS state — including the rustls state machine and ring-xous's transpiled bignum tables — lives in whichever process imports `libs/tls`. Sigchat linking its own rustls/ring-xous instance instantly *doubles* the TLS footprint (~700–800 KB duplicated). Second, **stack reservations are demand-paged on Xous.** Untouched stack pages cost no RSS; the cost of "many threads" is per-touched-stack-depth, not per-thread. This means the conventional "1 thread per concern" architecture is more affordable on Xous than on Cortex-M, but **code size is the binding constraint**, not stack reservation.

Sigchat itself is currently a skeleton (16 stars, 2 forks, README explicitly says "skeleton", no releases as of mid-2024) — the rebuild is happening at exactly the right moment to pick architecture rather than retrofit it.

## Proposal A — Keep PQXDH, hit 1.5 MiB anyway

The argument for keeping post-quantum: as of late 2024, Signal's servers reject `PreKeyBundle` uploads without `pqLastResortKey`/`pqOneTimeKeys`, and `CIPHERTEXT_MESSAGE_CURRENT_VERSION = 4` requires the Kyber payload (`rust/protocol/src/protocol.rs`). A PQ-less client cannot establish new sessions with current Signal users — it would talk only to a custom relay. **Proposal A is therefore the only path to a real Signal-compatible sigchat.** The cost: the ML-KEM-1024 implementation must shrink from ~493 KB to under ~150 KB to fit, and rustls/ring duplication must be eliminated.

### A1. Replace libcrux-ml-kem with RustCrypto's `ml-kem`

The single biggest win. `libcrux-ml-kem 0.0.8` (Signal's pinned version) is **17K SLoC** with a `build.rs` that auto-includes a portable plus an optimized backend by `CARGO_CFG_TARGET_ARCH` — on `riscv32imac` the AVX2/NEON paths should drop, but the user's measured 493 KB on x86_64 includes the SIMD path and even the portable path is ~200–250 KB. **RustCrypto `ml-kem 0.2.x`** (https://github.com/RustCrypto/KEMs/tree/master/ml-kem) is **~3K SLoC, single portable codepath, no `build.rs` SIMD scaffolding, no_std, MSRV 1.74**. Its source weighs 126 KB; built `.text` lands roughly **80–150 KB for ML-KEM-1024 only**. **Estimated saving: ~350 KB.**

Mechanically this is a fork-and-patch of `signalapp/libsignal/rust/protocol/src/kem` to swap the `pqcrypto-kyber`/`libcrux-ml-kem` import for `ml_kem::MlKem1024`. The crate API differs — RustCrypto uses `EncodedSizeUser` and `KemCore` traits — so a thin adapter (~100 LOC) is needed. Caveat: RustCrypto `ml-kem` has not been independently audited (per its README), whereas `libcrux-ml-kem` is formally verified via F\*. The threat-model concession is moving from a verified ML-KEM to a non-verified one.

A second variant — binding **PQClean's C reference for ML-KEM-1024 only** — yields the smallest possible footprint (~30–50 KB stripped per parameter set) but reintroduces a C toolchain to the SBOM. Sigchat's README already mentions GCC is needed for the ring build, so this is not a new dependency, but it cuts against bunnie's "Rust-only SBOM" goal.

### A2. Do not bundle a second rustls in sigchat

Confirm and enforce that sigchat's WebSocket transport routes through the existing Xous `lib/tls` rather than pulling its own `rustls` + provider. `tungstenite` 0.28 with `default-features = false` is **~30 KB and crypto-free**; only the optional `rustls-tls-*` and `native-tls` features pull a TLS stack. Use the bare `tungstenite` and feed/drain its byte stream into Xous's TLS IPC. **Estimated saving if currently duplicated: 200–800 KB.** This is also what `libsignal` itself avoids by linking `boring` (BoringSSL FFI), which is impractical to build on `riscv32imac-unknown-xous-elf` anyway — so sigchat is forced into this discipline regardless.

### A3. Migrate the Xous TLS service from ring-xous to rustls-rustcrypto

`ring-xous` (https://github.com/betrusted-io/ring-xous) is a c2rust-transpiled port of ring 0.16 — necessary in 2022 to get TLS on Xous without GCC, but it always ships every primitive (RSA, all NIST curves, AES, AES-GCM, ChaCha20-Poly1305, SHA-1/256/384/512, Ed25519, X25519, HMAC, HKDF, PBKDF2) and has no feature flags. **`rustls-rustcrypto`** (https://github.com/RustCrypto/rustls-rustcrypto) is a `CryptoProvider` built from the RustCrypto crates that the rest of the stack already pulls in (sha2, hmac, hkdf, aes-gcm, chacha20poly1305, x25519-dalek, p256). Switching the Xous TLS service to it lets the entire SBOM share one set of primitives — sigchat's libsignal-protocol AES/SHA/Curve25519 and the kernel's TLS AES/SHA/Curve25519 collapse into the same monomorphizations. **Estimated saving: 200–340 KB across the system.** Caveat: rustls-rustcrypto is at version 0.0.x, self-described as supporting "70% of cipher suite usage." Test against `chat.signal.org` before committing.

### A4. Strip libcrux-ml-kem to ML-KEM-1024 only, force portable

If A1 is not acceptable (verification regression too costly), the minimum action is `libcrux-ml-kem = { default-features = false, features = ["mlkem1024"] }` plus `LIBCRUX_DISABLE_SIMD256=1` in the build environment and a verification (`cargo bloat`/`twiggy`) that no AVX2 symbols leak into the RV32 binary. Estimated saving: 30–50% of libcrux's 493 KB ≈ **150–250 KB**. This is the cheap incremental win you take while doing A1.

### A5. Cull libsignal workspace members

`signalapp/libsignal` is a workspace; sigchat needs only `rust/protocol` (and possibly `rust/account-keys`, `rust/zkcredential`). Skipping `rust/net` (drops boring, hyper, tonic, tokio-tungstenite — sigchat brings its own thinner WS), `rust/keytrans`, `rust/attest` (drops SGX attestation), `rust/message-backup`, `rust/media` (drops mp4san/webpsan/mediasan), and `rust/zkgroup` (only if groups aren't a launch feature) cuts hundreds of KB of code that would otherwise be linked-in for unused features. There is **no upstream "lite" feature flag** — Issue #152 explicitly warns "use outside Signal not recommended" — but Cargo's per-member dependency selection gives this granularity for free.

### A6. Force the curve25519-dalek fiat backend

`RUSTFLAGS='--cfg curve25519_dalek_backend="fiat"'` selects the formally verified `fiat-crypto` backend and avoids any SIMD scaffolding. On `riscv32imac` the SIMD backend is unreachable anyway (it requires 64-bit words), but explicit selection prevents accidental regressions and gains formal verification of the field arithmetic. Saving: ~30 KB plus a verification upgrade.

### A7. Single-threaded async, one executor

The architectural rule for hitting the budget: **one OS thread runs an Embassy-style executor; a second OS thread (optional) handles UI**. Per Tweede Golf's STM32F446 benchmark and Memfault's analysis, Embassy tasks are state machines averaging "10s of bytes" each; SiliconWit's measurement: 10–20 tasks ≈ <5 KB total state vs FreeRTOS ≈ 40+ KB just for stacks. Embassy's built-in RISC-V thread executor is bare-metal-only and won't run inside a Xous user task, so a custom pender that calls `xous::wait_event` is needed (~150 LOC). The four Signal concerns (WS read, WS write, keepalive timer, send queue) become four spawned tasks on one executor — **the messaging architecture is one `select!` loop, not four threads.**

The Tighten Rust's Belt paper (Ayers et al., LCTES'22) measured futures at "100 bytes per future, 100–200 bytes per future overhead, 330 bytes per combinator" and a futures-based libtock-rs app was 80% larger than a non-futures equivalent — so async is not free. The win comes from collapsing thread *stacks* (each formerly ~64 KB reservation), not from the futures themselves. A budget of **2 OS threads × ~16 KB touched stack + ~5 KB futures state ≈ 40 KB** is a reasonable target; compare the alternative of 5 threads × 32 KB touched = 160 KB.

### A8. Allocator: TLSF as global, bumpalo arenas around bursts

Signal's allocation pattern is bursty: each message's Double Ratchet step allocates a few hundred bytes and frees them; each TLS handshake briefly allocates 9–50 KB of working state then releases it (Tasmota/BearSSL data shows TLS state is heavy *only during* handshake). Mainline Xous uses a generic `liballoc` over `IncreaseHeap`/`DecreaseHeap` syscalls. Switching the global allocator to **`embedded-alloc` with the `tlsf` (rlsf) backend** gives O(1) malloc/free and superior fragmentation behavior under bursty load (the rust-embedded/embedded-alloc PR #78 nRF52840 measurement: "TLSF was superior in latency and power consumption on all workloads, ~2-3% improvements after days of runtime"). On top of TLSF, **wrap each Signal-envelope encrypt and each TLS handshake in a `bumpalo::Bump` arena**, reset on completion — the arena absorbs the burst and releases it in one bump-pointer reset, removing fragmentation pressure on the global heap entirely. Use **`heapless::pool::Box`** for fixed-shape per-conversation ratchet state objects. None of this saves *code* size — it saves *peak* RSS during handshakes, which is what blows past 1.5 MiB even when the steady-state binary fits.

### A9. Hand-roll or micropb the Signal protobufs

`prost` generates `Option<T>`/`Vec<T>` everywhere and pulls `bytes`. Signal's wire schema is small and bounded — **Envelope, SignalMessage, PreKeySignalMessage, SenderKeyMessage, Content** plus a handful of websocket-message types. **`micropb`** (https://github.com/YuhanLiin/micropb) generates `no_std + no_alloc` code using `heapless::Vec` with compile-time fixed capacities and a hazzer bitfield for optional-field tracking — the same generator output but ~10–30 KB smaller per message set, and removes the `alloc` dependency from the protobuf path. **`femtopb`** explicitly markets itself as the "smallest footprint no-std no-alloc no-panic protobuf crate" and **`noproto`** (Embassy team) is "optimized for binary size, not performance." Hand-rolled decoding of the ~6 message types is also realistic at ~3–8 KB total for ~200 LOC, vs ~20–50 KB for prost-generated code. **Estimated saving: 20–50 KB.**

### A10. Build flags discipline

The `min-sized-rust` baseline profile is mandatory:

```toml
[profile.release]
opt-level = "z"
lto = "fat"
codegen-units = 1
panic = "abort"
strip = true
```

For `riscv32imac-unknown-xous-elf` you must rebuild std anyway (no precompiled std exists for that target). Make it pay: `-Z build-std=std,panic_abort -Z build-std-features="optimize_for_size,panic_immediate_abort"`. Per `min-sized-rust`, this reduces a hello-world from ~250 KB to **30 KB** (8 KB libc-only). For sigchat, expect a 30–50% binary reduction from these flags alone, but only if A11 (panic discipline) is paired with them.

### A11. Kill `core::fmt` reachability

James Munns's nrf52 measurement: a single `iprintln!("answer: {}", 42u8)` weighs **2,340 B**; without args, **314 B**; bare `loop{}`, **94 B**. Once any path reaches `core::fmt::write`, you pay the full ~6 KB floor (`Formatter::pad`, Unicode `is_printable` table, `slice_error_fail`, integer Debug impls). Ferrous Systems measured panic+log code at **13.85 KB → 1.59 KB** (9× reduction) by switching from `rtt_target::rprintln!("{:?}", info)` to defmt. The rule: **register a `panic_handler` that does not format `PanicInfo`**, replace `format!` with `write!` to a `heapless::String<N>`, drop `#[derive(Debug)]` from production builds (override with `unreachable!()` so LTO collapses to abort), and where logging is needed, use a defmt-style interning logger — emit log indices over a Xous message; resolve strings on the host via the ELF string table. The Tighten Rust's Belt paper ascribes **9.5% of the entire Ti50 binary** to `PanicInfo` alone; eliminating panic formatting saved 18,924 B / 24.9% of total project savings. **Estimated saving on sigchat: 50–150 KB.**

### A12. Generic monomorphization control

`cargo llvm-lines` finds methods whose monomorphizations dominate. The Tighten Rust's Belt paper measured Tock's `Grant::enter()` at 150 B per copy × 59 callsites = **~9 KB of one method**. The fix is the outer-generic-inner-concrete pattern: a generic outer that converts to a concrete type and calls a non-generic, `#[inline(never)]` inner. nickb.dev's serde measurement: introducing `erased-serde` to convert `T: Deserializer` to `&mut dyn Deserializer` at one boundary cut Wasm payload by **45%**. For sigchat, expect curve25519-dalek's `Scalar`/`EdwardsPoint` traits and libsignal's session-state generics to be the worst offenders — measure them, then box at the API boundary.

### A13. Allocate fixed-size cryptographic state on the stack

RustCrypto's `aes-gcm` exposes a `heapless` feature for in-place `encrypt_in_place(&nonce, &aad, &mut buffer)` over `heapless::Vec<u8, N>`, eliminating the heap from the AEAD path entirely. SHA-256/HMAC have fixed 64 B block sizes; Ed25519/X25519 keys are 32 B. With `GenericArray<u8, N>` and `heapless::Vec<u8, N>` everywhere, the Signal cryptographic core becomes purely stack-resident. This is critical because every heap trip during a handshake adds fragmentation pressure to the TLSF allocator and pushes peak RSS.

### A14. Restrict TLS cipher suites

Configure rustls in the Xous TLS service with **only `TLS_CHACHA20_POLY1305_SHA256`** (drop AES-GCM-SHA384 from TLS — the protocol envelopes still need AES, so AES code stays in the binary, but TLS doesn't have to add another GCM instance). Limit KX groups to `[X25519, X25519MLKEM768]`. Drop P-256/P-384/RSA. ChaCha20-Poly1305 is also ~3× faster than AES-bitsliced on a 32-bit RISC-V without AES extensions. **Estimated saving: 15–25 KB.**

## Proposal B — Drop PQ, what becomes possible, and why you probably can't ship it

PQXDH adds one Kyber-1024 encapsulation on send and decapsulation on receive, mixing the shared secret SS into the same KDF as X3DH (`SK = KDF(DH1 || DH2 || DH3 || DH4 || SS)`). It is a **handshake-only** upgrade — once a session is established the Double Ratchet runs identically. The wire delta: PreKeyBundle gains `pqLastResortKey` + `pqOneTimeKeys`; PreKeySignalMessage gains `kyber_pre_key_id` and `kyber_ciphertext` (1568 bytes each for ML-KEM-1024). `CIPHERTEXT_MESSAGE_CURRENT_VERSION = 4` (with Kyber); v3 (`CIPHERTEXT_MESSAGE_PRE_KYBER_VERSION`) is rejected on receive.

### What ripping PQ out actually saves

Drop `libcrux-ml-kem`, `pqcrypto-ml-kem`, the `kem` module, and the related serialization paths. **Estimated saving: 600–900 KB** including rodata, wrapper code, and the SPQR (`SparsePostQuantumRatchet`) ratchet — which Signal deploys *separately* from PQXDH for ~50-message-cadence in-session PQ refresh and would also be dropped. This is the single largest swing available, larger than any of A1–A14 individually.

### Why you can't ship it as a Signal-compatible client today

The Signal service has rejected non-PQ PreKeyBundles for new sessions since 2024 (per Signal's deployment plan announced Sept 2023). There is **no protocol negotiation downgrade** for PQXDH — version 4 is the only version accepted. There is **no per-message or per-conversation runtime fallback**: PQXDH is a handshake-time upgrade, not an envelope-time toggle. There is **no upstream feature flag** in `signalapp/libsignal` to omit Kyber; the protocol struct allows `kyber_payload: Option<KyberPayload>` but the receive path explicitly rejects `None`. Patching this out is a ~50 LOC fork in `rust/protocol/src/protocol.rs`, but the resulting client cannot establish sessions with current Signal users.

### When Proposal B is actually useful

Three scenarios make it real:

A non-Signal relay that speaks the Signal wire protocol but with PQ optional — this would be a research/sandbox deployment, not the production Precursor messenger.

A future Signal RFC (analog of libsignal PR #284 by Whisperfish's @rubdos, which set a precedent for accepting build-time flags useful to non-Signal clients) that exposes a `pqxdh` Cargo feature gated on by default. There is no public roadmap for this.

An interim emergency-fit position where sigchat ships without PQ as a tech preview, talks to a custom relay, and the production posture is "Proposal A as soon as A1–A4 land us under budget." This is the realistic role of Proposal B in the project plan.

### Threat-model concession

The cost of dropping PQ is **harvest-now-decrypt-later (HNDL)** exposure: an adversary who records ciphertext today and obtains a cryptographically-relevant quantum computer in 10–20 years can decrypt the captured handshakes and from there decrypt the Double Ratchet's initial messages. Forward secrecy and post-compromise security against *classical* attackers are preserved. For a high-assurance device targeting activists/journalists, this is exactly the threat PQXDH was designed to address. Precursor's stated threat model is nation-state-resistant — these are precisely the users who care about HNDL — so Proposal B is a compromise that the project's intended audience is unlikely to accept as a permanent posture.

## Reference clients give us a sanity check

`libsignal-protocol-c` (deprecated, no PQXDH) ships at **~200–300 KB stripped** — proof a Signal-protocol implementation can be small once you remove ML-KEM. signal-cli's `libsignal_jni.so` (with BoringSSL) is **~30–50 MB**. Signal Desktop runs **350 MB – 2.5 GB resident**. Whisperfish on Sailfish OS runs at 100–300 MB RSS but that's dominated by Qt, not crypto. **No public Signal-protocol implementation for microcontrollers, smartcards, or RTOS targets exists** — sigchat is the closest. No public fork of `signalapp/libsignal` aimed at size reduction was found across GitHub, lobste.rs, or the Rust forums.

This is consistent with the conclusion: **the size discipline must be built inside sigchat**, not borrowed from a "minimal libsignal" that doesn't exist.

## Architectural changes ranked by impact

| # | Change | Estimated saving | Effort | Risk | Maintenance |
|---|---|---|---|---|---|
| 1 | Don't bundle a 2nd rustls in sigchat (route WS through Xous TLS) | 200–800 KB | 1 week | Low (proven pattern) | Low |
| 2 | libcrux-ml-kem → RustCrypto ml-kem | 350 KB | 1–2 weeks | Medium (audit gap) | Low (wrapper ~100 LOC) |
| 3 | Migrate Xous TLS to rustls-rustcrypto | 200–340 KB | 2–3 weeks | Medium (0.0.x, test against Signal) | Medium |
| 4 | Build flags: opt=z + LTO + codegen=1 + panic=abort + build-std + immediate-abort | 30–50% of binary | 1–2 days | Low | Low |
| 5 | Kill `core::fmt` reachability (panic_handler, no Debug, defmt-style logging) | 50–150 KB | 1 week | Low–Medium (debuggability) | Medium (lint/CI) |
| 6 | Cull libsignal workspace members (skip net/keytrans/attest/media/zkgroup) | 100–300 KB | 3–5 days | Medium (must replace net/zkgroup) | Low |
| 7 | Single-threaded async with custom Xous pender | 50–150 KB stacks/futures vs threads | 2 weeks | Medium (new pattern in Xous) | Medium |
| 8 | TLSF global + bumpalo arenas + heapless::pool | Peak RSS during handshake | 1 week | Low | Low |
| 9 | Hand-roll/micropb the Signal protobufs | 20–50 KB | 1 week | Low | Medium (schema drift) |
| 10 | Generic monomorphization control (outer/inner pattern, dyn at boundaries) | 30–80 KB | Ongoing | Low | Ongoing discipline |
| 11 | Force fiat backend for curve25519-dalek | 30 KB + verification upgrade | 1 day | Low | Low |
| 12 | Strip libcrux to mlkem1024 only (incremental, before #2 lands) | 150–250 KB | 1 day | Low | Low (interim) |
| 13 | Restrict TLS cipher suites to ChaCha20-Poly1305 + X25519MLKEM768 | 15–25 KB | 1 day | Low (verify Signal still negotiates) | Low |
| 14 | Stack-allocate crypto state (heapless feature on aes-gcm, GenericArray everywhere) | Peak RSS, no .text | 1 week | Low | Low |
| **B-only** | Drop PQXDH (Proposal B) | 600–900 KB | 1–2 weeks fork | **Server incompatibility** | High (fork drift) |

## Recommended ordered path with budget gates

The discipline is "every change has a verifiable RSS gate; no change ships without measurement."

**Phase 0 — Measure** (3 days). Produce a baseline `cargo bloat --release --target riscv32imac-unknown-xous-elf -n 100`, `twiggy top -n 50`, and `cargo llvm-lines` for sigchat as it stands. Establish the actual current RSS on hardware (the 3.9 MB number is x86_64 hosted — RV32 may be smaller already because AVX2 paths drop). **Gate: a known starting number.**

**Phase 1 — Cheap wins** (1 week). Apply the build-flag profile (#4), force the curve25519-dalek fiat backend (#11), strip libcrux features to `mlkem1024` only (#12), restrict TLS cipher suites (#13). **Gate: −300 to −500 KB vs Phase 0.**

**Phase 2 — TLS deduplication** (1 week). Verify and enforce that `tungstenite` is configured without TLS features and that sigchat's WS goes through Xous's `lib/tls` (#1). If the current build *isn't* duplicating, this is a no-op CI guard; if it is, it's the largest single saving in the project. **Gate: confirm one TLS instance system-wide.**

**Phase 3 — Replace ML-KEM** (2 weeks). Fork `rust/protocol/src/kem`, swap `libcrux-ml-kem` for RustCrypto `ml-kem` (#2). Land an interop test against a captured Signal handshake before merging. **Gate: −350 KB; interop tests pass.**

**Phase 4 — Kill `fmt` reachability** (1 week). Custom panic_handler, `-Cpanic=immediate-abort`, drop production `#[derive(Debug)]`, replace `format!` with `write!`+`heapless::String`, introduce a defmt-style interning logger (#5, #11). **Gate: −50 to −150 KB.**

**Phase 5 — Allocator and async architecture** (2–3 weeks). Switch global to `embedded-alloc` TLSF; add `bumpalo` arenas around handshake/encrypt; use `heapless::pool::Box` for ratchet state (#8); move to a single-thread async architecture with a custom Xous pender (#7). **Gate: peak RSS during a TLS handshake is < 250 KB above steady-state.**

**Phase 6 — Cull libsignal members** (1 week). Disable `rust/net`, `rust/keytrans`, `rust/attest`, `rust/message-backup`, `rust/media`, `rust/zkgroup` (#6). Replace `rust/net` with a thin sigchat-internal HTTP/WS client. **Gate: −100 to −300 KB.**

**Phase 7 — Migrate Xous TLS provider** (2–3 weeks, can run in parallel with 5–6). Switch `lib/tls` from `ring-xous` to `rustls-rustcrypto` (#3). This is system-wide, so coordinate with vault/shellchat. **Gate: −200 to −340 KB system-wide; vault and shellchat still work.**

**Phase 8 — Protobuf and ongoing discipline** (1 week + ongoing). Migrate to `micropb` for Signal messages (#9). Establish a rule that every PR runs `cargo bloat`/`twiggy` and reports the delta; reject PRs that grow .text by more than the budget allows (#10, #14). **Gate: −20 to −50 KB; per-PR size delta in CI.**

**Cumulative expected outcome**: from a starting ~3.9 MB hosted (likely 2.5–3 MB on RV32 due to AVX2 drop), Phases 1–7 should land in **1.4–1.8 MiB**. The 1.5 MiB target is achievable with Phases 1–4 + 6 alone if Phase 1 reveals existing duplication; Phases 5, 7, 8 are the ones that *keep* it small as features land.

## How to keep it small forever

The deepest insight from the Tighten Rust's Belt paper is that 76 KB / 19% of Google's Ti50 firmware came from "hidden" sources: panic info, vtables, static initializers, monomorphization. A messaging client adds features over time — group chat, sealed sender, story messages, voice — and each one risks pulling unbounded transitively. Three architectural disciplines hold the line:

The first is **a single async executor and a fixed thread budget**. Adopting one OS thread for the executor and one for UI and refusing to spawn more is a structural commitment. Every new feature must fit on the existing executor as a new task; nothing gets its own thread.

The second is **a single shared crypto provider across the SBOM**. Picking RustCrypto and configuring `rustls-rustcrypto` to use those primitives means sigchat's libsignal AES/SHA/Curve25519 and the Xous TLS service's AES/SHA/Curve25519 dedupe at the linker. A new feature that needs HMAC-SHA-512 must reuse the existing `sha2` instance, not bring its own.

The third is **a per-PR `.text` budget**. The xtask comment "essential services must be <1 MiB" is the model. Make it a CI gate that fails any PR whose `cargo bloat` delta exceeds a threshold (say 5 KB) without an explicit waiver and a feature-flag justification. The Tighten Rust's Belt paper, James Munns's `fmt` measurements, and the defmt 9× saving all suggest that without this gate, code size monotonically grows because every developer adds one `Debug` derive at a time. Sigchat's right answer is to start the project with this gate in place rather than retrofit it after the budget breaks.

The conclusion is that 1.5 MiB is achievable for a PQ-preserving Signal client on Precursor, but it requires architectural choices — single TLS instance, RustCrypto-only crypto provider, single executor, fmt-free panic path — that are decided once at the start of the project and reinforced by measurement. Proposal A is the path; Proposal B is a fallback that the project's threat model probably won't accept; the real engineering discipline is the third axis: keeping it small as features land.
