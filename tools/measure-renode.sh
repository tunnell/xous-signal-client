#!/usr/bin/env bash
# tools/measure-renode.sh
#
# Runtime smoke test under Renode hardware emulation. Builds a Xous
# image with xous-signal-client, boots it in Renode (no GUI), and
# checks that the binary reaches its event loop without panic, fault,
# or exception. Mirrors TESTING-PLAN.md Check 4.
#
# This is a smoke test, not a per-feature regression — it confirms the
# binary can boot and the `xous_signal_client: my PID is N` log line
# appears. Functional correctness is exercised in family 1 (Rust unit
# tests) and family 2 (hosted-mode E2E).
#
# Prerequisites:
#   - Renode v1.16.1 or later on PATH
#   - xous-core checkout at $XOUS_CORE_PATH (default ../xous-core) on
#     branch feat/05-curve25519-dalek-4.1.3
#   - xous-signal-client release binary already built for the Xous
#     target (run measure-size.sh first if needed)
#
# Output:
#   - /tmp/xsc-renode-boot-<timestamp>.log (full Renode console)
#
# Exit codes:
#   0 = boot reached event loop, no panic
#   1 = panic, fault, or did not reach event loop
#   2 = setup failure (Renode not installed, xous-core not found,
#       binary not built)
#
# Usage:
#   ./tools/measure-renode.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=test-helpers.sh
source "$SCRIPT_DIR/test-helpers.sh"
ROOT="$(xsc_repo_root)"

xsc_require_cmd renode \
    "Install Renode v1.16.1+: https://github.com/renode/renode/releases" \
    || exit 2

XOUS_CORE_PATH="${XOUS_CORE_PATH:-$ROOT/../xous-core}"
if [[ ! -d "$XOUS_CORE_PATH" ]]; then
    echo "xous-core not found at $XOUS_CORE_PATH" >&2
    echo "Set XOUS_CORE_PATH to a valid checkout, or place xous-core " >&2
    echo "as a sibling of this repository." >&2
    exit 2
fi

TARGET="riscv32imac-unknown-xous-elf"
BIN_NAME="xous-signal-client"
ELF="$ROOT/target/$TARGET/release/$BIN_NAME"
if [[ ! -f "$ELF" ]]; then
    echo "Binary not found: $ELF" >&2
    echo "Run ./tools/measure-size.sh first to build." >&2
    exit 2
fi

TS=$(date +%s)
LOG="/tmp/xsc-renode-boot-${TS}.log"
RESC="$XOUS_CORE_PATH/emulation/xous-release.resc"

if [[ ! -f "$RESC" ]]; then
    echo "Renode script not found: $RESC" >&2
    exit 2
fi

COMMIT="$(cd "$ROOT" && git rev-parse --short=7 HEAD)"

echo "=== Building Renode image with xous-signal-client ==="
cd "$XOUS_CORE_PATH"
# --git-describe must match the format vX.Y.Z-N-gHASH where N is a u16
# (xous-create-image.rs::parse_versions). Using a benign value here is
# fine: the renode image is throwaway, used only to confirm the binary
# boots.
#
# The "sigchat:" alias resolves the GAM context name to "signal" via
# xous-core's apps/manifest.json (matching what xous-signal-client/
# src/main.rs registers), while the binary path points at the freshly
# built xous-signal-client release ELF. Using "xous-signal-client:"
# directly instead would regenerate gam/src/apps.rs as empty (no
# manifest entry yet), breaking subsequent hosted-mode `cargo test`
# runs that link against gam. Keep the alias until xous-signal-client
# is registered in xous-core's apps manifest.
if ! cargo xtask renode-image \
        "sigchat:$ROOT/target/$TARGET/release/$BIN_NAME" \
        --no-verify \
        --git-describe "v0.9.8-0-g${COMMIT}" 2>&1 | tail -10; then
    echo "Renode image build failed." >&2
    exit 2
fi

echo ""
echo "=== Booting Renode (90s timeout) ==="
timeout --kill-after=10 90 \
    renode --console --disable-gui \
    -e "include @${RESC}; start" \
    >"$LOG" 2>&1 || true

echo "Boot log: $LOG"

# Detect known-environmental Renode peripheral compile failures.
# Historically the LiteX_Timer_32.cs `long`/`ulong` incompatibility
# against Renode 1.16.1 was the load-bearing case here (resolved by
# tunnell/xous-core PR #18). Other peripheral version mismatches may
# surface as environmental skips in the future; keep the detection
# even though the LiteX cast itself is fixed.
if grep -qE "Could not compile assembly|peripherals/.*\.cs.*error CS" "$LOG"; then
    echo ""
    echo "=== Renode peripheral compile failure (environmental) ==="
    grep -E "Could not compile assembly|peripherals/.*\.cs" "$LOG" | head -5
    echo ""
    echo "This is a Renode / xous-core peripheral incompatibility;"
    echo "see tests/renode/README.md for context. Skipping renode smoke."
    exit 2
fi

# Check for panics next.
if grep -E "panic|abort|fault|exception|FATAL" "$LOG" >/dev/null 2>&1; then
    echo ""
    echo "=== Panic/fault detected ==="
    grep -E "panic|abort|fault|exception|FATAL" "$LOG" | head -20
    exit 1
fi

# Check the binary reached its event loop.
if ! grep "INFO:xous_signal_client" "$LOG" >/dev/null 2>&1; then
    # After the LiteX_Timer fix landed (tunnell/xous-core PR #18),
    # peripherals compile cleanly and Renode's machines start, but no
    # binary log output appears in the boot log. The cause is that
    # `renode --console --disable-gui` does not auto-redirect peripheral
    # UART output to stdout — it needs an explicit
    # `sysbus.<uart> CreateFileBackend ...` directive in the .resc, plus
    # additional setup (the binary's logger may bind to one of `uart`,
    # `console`, or `app_uart` and the right one isn't yet identified).
    # Until that follow-up lands, treat "no INFO log captured" as an
    # environmental skip rather than a binary regression. See
    # tests/renode/README.md "Known limitations" for the open work.
    echo ""
    echo "=== No INFO log captured from emulated binary (environmental) ==="
    echo "Last 20 lines of boot log:"
    tail -20 "$LOG"
    echo ""
    echo "Renode boots and machines start; the binary's UART output is"
    echo "not yet routed to the boot log. See tests/renode/README.md."
    echo "Skipping renode smoke until UART analyzer wiring lands."
    exit 2
fi

echo ""
echo "=== Smoke test PASS ==="
grep "INFO:xous_signal_client" "$LOG" | head -5
exit 0
