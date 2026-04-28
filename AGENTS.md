# AGENTS.md

Cold-start context for AI agents and human contributors working on
`xous-signal-client`.

## What this is

A Signal Protocol messaging client for [Precursor](https://www.crowdsupply.com/sutajio-kosagi/precursor)
running on [Xous OS](https://github.com/betrusted-io/xous-core).
1:1 messaging end-to-end against live `chat.signal.org`. Hand-rolled
libsignal-protocol orchestration (PDDB-backed protocol stores, worker-
thread WebSocket, multi-device fan-out, sync transcripts).

This project shares its in-tree app name with `sigchat`, a Xous chat-app
skeleton in `xous-core`'s app registry. `xous-signal-client` began as
a fork of that skeleton and diverged completely as the Signal protocol
implementation grew; the `sigchat` name is preserved internally to
xous-core to avoid changes to the app-allowlist, but the codebases are
now distinct. Practical implications:

- The `cargo xtask run sigchat:...` invocation, the
  `gam::APP_NAME_SIGCHAT` constant, the `SigchatOp::*` IPC opcode set,
  and the `sigchat.*` PDDB dict prefixes (e.g., `sigchat.account`,
  `sigchat.session`) all use that in-tree app name. Do not rename
  these in this repo.
- Sister-folder working notes at `~/workdir/xous-signal-client-notes/`
  preserve the project's debugging history, bug arcs, and
  lessons-learned. Read `activeContext.md` there at the start of a
  new session.

## Required environment

- Rust toolchain. Hosted-mode builds work with stable 1.95+. Cross-
  compile to `riscv32imac-unknown-xous-elf` uses xous-core's xtask
  toolchain (downloads a betrusted-io fork of the Rust compiler with
  pre-built std).

- **`xous-core` checked out on branch `feat/05-curve25519-dalek-4.1.3`.**
  This is non-negotiable — other branches pin `root-keys` to
  `curve25519-dalek = "=4.1.2"` which conflicts with this project's
  requirement of 4.1.3. The branch carries several local patches; see
  `~/workdir/xous-signal-client-notes/techContext.md` for the patch
  table with upstream-PR tracking.

- **`signal-cli`** for end-to-end testing. Linked to one of the test
  accounts as a secondary device.

- **Working `tools/.env`** (gitignored). Copy `tools/test-env.example`
  and edit. Real account values never go in committed files.

- **PDDB snapshot** of a linked account at
  `~/workdir/xous-core/tools/pddb-images/hosted-linked-display-verified.bin`
  (or equivalent post-prekey-persistence-fix snapshot).

  **Warning:** the `.bin` snapshot files contain real session keys
  and identity material. They are gitignored; do not commit them or
  paste their contents into any external system (issue tracker, chat,
  log). Treat them as sensitive credential material.

## Build

```bash
# Hosted (for tests + scan automation)
cd ~/workdir/xous-signal-client
cargo build --release --features hosted

# Cross-compile to the device target
cargo build --release --target=riscv32imac-unknown-xous-elf \
    --bin xous-signal-client --features precursor
```

`hosted` and `precursor` are mutually exclusive. `hosted` activates IPC
stubs for testing on Linux; `precursor` activates the real hardware
configuration.

The `hosted` feature must **omit** `gam/hosted` — forwarding it causes
an infinite `register_ux: lend_mut` loop (IPC format mismatch).

## Run hosted

```bash
cd ~/workdir/xous-core
cargo xtask run sigchat:../xous-signal-client/target/release/xous-signal-client
# Note: uses `sigchat:` xtask alias — xous-signal-client is registered
# in xous-core's app-allowlist under that name (see "What this is").
```

## Testing

Three test families exist; see `tests/README.md` for full
documentation.

- `cargo test --features hosted` — fast Rust unit tests
- `./tools/run-all-tests.sh` — full orchestrator (Rust + hosted
  E2E + memory footprint), with `KNOWN_FAIL` status surfacing
  documented protocol gaps without blocking
- `./tools/measure-renode.sh` — runtime memory measurement under
  Renode hardware emulation. Renode simulates the Precursor RV32
  target; this is the path for hardware-style testing without a
  physical device. State of Renode infrastructure is documented
  in `tests/README.md`.

Hosted mode is the primary iteration loop. Renode is the gate
before declaring something works on hardware-equivalent.

`KNOWN_FAIL` (exit 87 from `scan-send.sh`) is non-blocking; see
`tests/known-issues.md`.

For methodology, read `tests/README.md`. Six principles from the
Phase A debugging arc are codified there.

## Diagnostic instrumentation

Two env vars enable detailed logging without affecting production logs:

- `XSCDEBUG_DUMP=1` — wire-byte capture in `outgoing.rs` →
  `/tmp/xsc-wire-dump.txt`. Read via `tools/decode-wire.sh`.
- `XSCDEBUG_RECV=1` — `[recv-debug]` log line in `main_ws.rs` showing
  body, author, timestamp on inbound messages.

Both default to off; production logs remain body-free.

## Never do

- **Never push to `betrusted-io/*`.** All git operations target
  `tunnell/*`. Upstream PRs are a human decision.
- **Never commit real account data.** No phone numbers, no UUIDs, no
  test-message strings that have appeared in prior sessions. Use
  placeholders (`+15550100`, all-zero UUID) in any file outside
  `tools/.env`.
- **Never commit a PDDB snapshot.** They contain session keys and
  identity material. The `.gitignore` covers `**/pddb-images/*.bin`
  and `**/pddb-images/*.snapshot`.
- **Never add brand attribution in commit trailers.** Convention:
  `Generated with an AI agent.` + `Signed-off-by:` (DCO).
- **Never run destructive git commands without per-command approval.**
  Force-push only with `--force-with-lease` and only with explicit
  authorization for the specific operation.
- **Never declare success based on log lines alone.** The "200 OK / post
  sent / receipt envelope" pattern is the project's textbook anti-pattern.
  Verification requires the three-legged stool: wire bytes + recipient
  parse + user-visible. See `tests/README.md` Principle 3 and
  `~/workdir/xous-signal-client-notes/lessons-learned.md`.

## Reporting protocol

Each non-trivial session produces:
- **A session report** (markdown, plain prose, no marketing voice)
  describing what changed, what was verified, what was deferred,
  what's open. Archived in
  `~/workdir/xous-signal-client-notes/_archive/REPORTS/` following the
  convention `YYYY-MM-DD-<topic>.md` for chronological sortability.

These are working notes, not part of this repo.

## Maintenance contract

Documentation in this repository is maintained as part of code
changes, not as a separate effort. Every session that lands a
change is responsible for keeping documentation aligned with
the code:

1. **Code changes that affect public behavior** must update
   `CHANGELOG.md` under `[Unreleased]` in the same PR.
2. **Architectural changes** (new modules, changed module
   boundaries, new dependencies, changed data flow) must update
   `ARCHITECTURE.md` and add or supersede an ADR in
   `docs/decisions/` in the same PR.
3. **Bug fixes that resolve issues documented in
   `tests/known-issues.md` or
   `xous-signal-client-notes/bug-arcs/`** must update those documents
   to reflect the resolution in the same PR.
4. **Test changes** must keep `tests/README.md` accurate (test
   counts, family descriptions, KNOWN_FAIL state).
5. **Build / dependency changes** must update the relevant
   sections of this file (AGENTS.md) and `techContext.md` in the
   notes folder.
6. **Every session ends with `./tools/run-all-tests.sh`
   reporting green** (PASS or SKIPPED for each family;
   KNOWN_FAIL is acceptable for documented issues).

The check at the end of each session is: did the docs change
in step with the code, or did the code drift away from what the
docs describe? If the latter, fix it before committing.

## Documentation

- **AGENTS.md** (this file) — cold-start context.
- **README.md** — for human readers; brief project description, build
  instructions, contribution path.
- **ARCHITECTURE.md** — bird's-eye view of major modules.
- **CHANGELOG.md** — what's shipped (Keep a Changelog format).
- **docs/decisions/** — architectural decision records (MADR format,
  immutable, append-only).
- **tests/README.md** — testing methodology + six principles.
- **tests/known-issues.md** — KNOWN_FAIL items with debugging starting
  points.

For institutional memory (bug arcs, lessons, things tried, debugging
history), see the sister local directory
`~/workdir/xous-signal-client-notes/`.

## Quick reference — repo state at consolidation time

- Branch: `chore/consolidation` (this PR).
- Most recent merged PR: #4 (scan-send leg-2 + KNOWN_FAIL convention).
- Tests: 65 passing (cargo test --features hosted), with additional
  unit tests being ported in this consolidation PR.
- Total binary: 4.0 MiB on Xous target (intentional; ~268% of 1.5 MiB
  hard target until Phase G size reductions land).
- KNOWN_FAIL: B2 (signal-cli libsignal decrypt fail on post-409-retry
  CIPHERTEXT). See `tests/known-issues.md`.
