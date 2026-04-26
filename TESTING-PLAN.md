# xous-signal-client testing discipline

Before any session reports "done" on a code change, run these checks
in order. Skip nothing without explicit human approval.

## What this is

A standard set of verifications to run after every significant code
change, until proper CI catches all of them automatically. Some
checks duplicate what CI will eventually do — that's intentional.
Local checks are immediate; CI may not run for hours if billing or
runner issues delay it.

## Pre-flight (every session)

```bash
cd ~/workdir/xous-signal-client

# Repo hygiene
git remote get-url origin   # must be tunnell/xous-signal-client
git branch --show-current   # confirm intended branch
git status                  # working tree state matches expectations

cd ~/workdir/xous-core
git branch --show-current   # must be feat/05-curve25519-dalek-4.1.3
```

If anything looks wrong, stop and report before doing anything else.

**xous-core branch is critical.** Other branches pin `root-keys` to
`curve25519-dalek = "=4.1.2"` while this project's patched fork
provides 4.1.3 — cargo fails with a dependency resolution error that
looks like a project bug but is environment misconfiguration. If on
the wrong branch:

```bash
cd ~/workdir/xous-core
git checkout feat/05-curve25519-dalek-4.1.3
```

## Required checks before reporting "done"

### Check 1: Build succeeds for Xous target

**Prerequisite:** xous-core on `feat/05-curve25519-dalek-4.1.3` (see Pre-flight).

```bash
cd ~/workdir/xous-signal-client
cargo build --release \
    --target=riscv32imac-unknown-xous-elf \
    --bin xous-signal-client \
    --features precursor 2>&1 | tail -30
echo "Exit: $?"
```

Must exit 0. Any compilation error is a stop-the-session blocker.

### Check 2: Size budget passes (or fails as expected)

```bash
python3 .github/scripts/check_size_budget.py \
    --budget .size-budget.toml \
    --binary target/riscv32imac-unknown-xous-elf/release/xous-signal-client \
    --target riscv32imac-unknown-xous-elf \
    --bin-name xous-signal-client \
    --features precursor \
    --report-md /tmp/local-budget-report.md
echo "Exit: $?"
cat /tmp/local-budget-report.md
```

**Block only on dramatic regressions: any per-crate cap breach AND that
crate grew ≥30% over its previous measurement.** Smaller per-crate deltas
are normal feature growth — surface the delta in the session report and
proceed. The TOTAL is already over the 1.5 MiB hard limit (intentional
until size-reduction work lands); a TOTAL breach by itself is not a
blocker.

The session report MUST always include the total binary size as a
top-level reported number, regardless of pass/fail status. This is how
binary growth gets tracked over time.

Concretely:
- Exit 0: all caps pass. Report total + note "all caps green".
- Exit 1, TOTAL over: report total + delta from previous baseline.
  Continue.
- Exit 1, per-crate over with growth <30%: report which crate, prior
  vs current measurement, percent growth. Continue.
- Exit 1, per-crate over with growth ≥30%: STOP. The session caused a
  size regression that needs investigation before commit.

When a per-crate cap is exceeded by normal growth, bump the cap to
`measured + 30% headroom` in `.size-budget.toml` as part of the same
session.

### Check 3: i686 sanity build (catches pointer-width regressions)

```bash
cd ~/workdir/xous-signal-client
RUSTFLAGS="-C relocation-model=static" \
cargo build --release \
    --target=i686-unknown-linux-gnu \
    --bin xous-signal-client \
    --features hosted \
    2>&1 | tail -20
echo "Exit: $?"
```

Note: this requires gcc-multilib installed (one-time `sudo apt
install gcc-multilib g++-multilib`). If not present, skip with a
note in the report — don't auto-install.

Must exit 0. A new compilation error here means a 32-bit-incompatible
pattern slipped in. Stop and report.

### Check 4: Renode boot smoke test (if changes touch runtime code paths)

Run only when the session's changes plausibly affect runtime
behavior (Cargo deps, startup/init, crypto, IPC, allocator). Skip
for trivial changes (warning cleanup, docs, comments).

```bash
cd ~/workdir/xous-core
COMMIT=$(cd ~/workdir/xous-signal-client && git rev-parse --short=7 HEAD)
cargo xtask renode-image \
    xous-signal-client:../xous-signal-client/target/riscv32imac-unknown-xous-elf/release/xous-signal-client \
    --no-verify \
    --git-describe "v0.9.8-791-g${COMMIT}" 2>&1 | tail -10

RESC=~/workdir/xous-core/emulation/xous-release.resc
timeout --kill-after=10 60 \
    renode --console --disable-gui \
    -e "include @${RESC}; start" \
    2>&1 | tee /tmp/renode-boot.log

grep -E "panic|abort|fault|exception|FATAL" /tmp/renode-boot.log
# No output = pass. Any output = panic = stop.

grep "INFO:xous_signal_client" /tmp/renode-boot.log | head
# Expect: "INFO:xous_signal_client: my PID is N" and chat lib startup.
```

If panics appear: stop, capture, report. Do not fix.
If the binary doesn't reach the event loop: stop, capture, report.

### Check 5: report what was verified

In the session's final report, include a "Verification" section:

```markdown
## Verification
- Xous build: pass / fail (with exit code)
- Size budget: pass / fail-as-expected (with delta from previous)
- i686 sanity: pass / skipped (reason) / fail
- Renode smoke: pass / skipped (reason) / fail-with-panic
```

## What this discipline buys

- Catches RV32-incompatible code immediately (Check 3)
- Catches size regressions immediately (Check 2)
- Catches runtime-breaking changes immediately (Check 4)
- Builds a habit so when CI does run, it confirms what's already
  known rather than discovering surprises

## When to skip checks

Skipping is allowed but must be explicit and justified:

- Check 4 (Renode): skip for trivial changes, document the skip
- Check 3 (i686): skip if gcc-multilib not installed, document
- Check 2 (size): never skip
- Check 1 (Xous build): never skip — this is the project's heartbeat

## Once CI is live

When tunnell/xous-signal-client's GitHub Actions are running,
Checks 1–3 will run automatically on every push. Local execution
becomes a fast-path to catch issues before push, not the primary
verification.

Check 4 (Renode) is unlikely to ever be in CI — it's heavyweight
enough that it makes sense as a periodic spot check.

## What this does NOT cover

Out of scope for the standard plan:

- Real network behavior (TAP setup needed, not in standard plan)
- pddb behavior with real hardware flash timing
- Hardware peripheral correctness (TRNG, EC, WF200)
- TLS handshake against real Signal servers

These need physical hardware and are deferred.
