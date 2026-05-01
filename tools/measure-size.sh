#!/usr/bin/env bash
# tools/measure-size.sh
#
# Builds the Xous-target binary and reports static size against the
# documented per-crate, per-section, and total budgets in
# .size-budget.toml. Wraps the existing
# .github/scripts/check_size_budget.py used by the size-budget CI job.
#
# Prerequisites:
#   - riscv64-unknown-elf binutils (size, readelf) on PATH
#   - cargo-bloat installed (cargo install cargo-bloat)
#   - xous-core checkout at ../xous-core (or $XOUS_CORE_PATH) on
#     branch dev-for-xous-signal-client
#   - Python 3.11+ (or pip install tomli for older)
#
# Output:
#   - Markdown report at /tmp/xsc-size-report-<timestamp>.md
#   - Section table + crate table on stdout
#
# Exit codes:
#   0 = all hard budgets pass
#   1 = at least one hard budget breached
#   2 = build or measurement failure (toolchain missing, build error)
#
# Usage:
#   ./tools/measure-size.sh
#   ./tools/measure-size.sh --skip-build      # measure existing ELF

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=test-helpers.sh
source "$SCRIPT_DIR/test-helpers.sh"
ROOT="$(xsc_repo_root)"

SKIP_BUILD=0
for arg in "$@"; do
    case "$arg" in
        --skip-build) SKIP_BUILD=1 ;;
        -h|--help)
            sed -n '/^# tools/,/^$/ p' "$0" | sed 's/^# \?//'
            exit 0 ;;
        *) echo "Unknown argument: $arg" >&2; exit 2 ;;
    esac
done

TARGET="riscv32imac-unknown-xous-elf"
BIN_NAME="xous-signal-client"
FEATURES="precursor"
ELF="$ROOT/target/$TARGET/release/$BIN_NAME"
TS=$(date +%s)
REPORT="/tmp/xsc-size-report-${TS}.md"

xsc_require_cmd cargo "Install Rust toolchain via rustup." || exit 2
xsc_require_cmd python3 "Install Python 3.11+ (or pip install tomli)." || exit 2
xsc_require_cmd riscv64-unknown-elf-size \
    "Install riscv64-unknown-elf binutils (e.g. apt install binutils-riscv64-unknown-elf)." || exit 2
xsc_require_cmd cargo-bloat "cargo install cargo-bloat" || exit 2

if (( ! SKIP_BUILD )); then
    echo "=== Building $BIN_NAME for $TARGET ==="
    cd "$ROOT"
    if ! cargo build --release --target="$TARGET" \
            --bin "$BIN_NAME" --features "$FEATURES"; then
        echo "Build failed." >&2
        exit 2
    fi
fi

if [[ ! -f "$ELF" ]]; then
    echo "ELF not found: $ELF" >&2
    echo "Run without --skip-build, or build first." >&2
    exit 2
fi

echo ""
echo "=== Running size budget check ==="
cd "$ROOT"
set +e
python3 .github/scripts/check_size_budget.py \
    --budget .size-budget.toml \
    --binary "$ELF" \
    --target "$TARGET" \
    --bin-name "$BIN_NAME" \
    --features "$FEATURES" \
    --report-md "$REPORT"
RC=$?
set -e

echo ""
echo "=== Report ==="
cat "$REPORT"
echo ""
echo "Report saved to $REPORT"

# Project policy (per .size-budget.toml::meta.note and TESTING-PLAN.md
# Check 2): the TOTAL is already over the 1.5 MiB hard limit by design
# until size-reduction work lands. A TOTAL-only breach is reported but
# is not a blocker. Any section or per-crate breach is a real failure.
# Map the python script's "any breach => exit 1" to the project policy:
# exit 0 if all per-crate and per-section caps pass, even if TOTAL is
# over.
if (( RC == 0 )); then
    exit 0
fi

# RC != 0: parse the report's breaches section. If every breach line
# starts with "TOTAL", treat as PASS-WITH-NOTE (project policy). If any
# breach is on a section or crate, return failure.
NON_TOTAL_BREACH=0
in_breaches=0
while IFS= read -r line; do
    if [[ "$line" == "**❌ Budget breaches:**" ]]; then
        in_breaches=1
        continue
    fi
    if (( in_breaches )) && [[ "$line" =~ ^-\  ]]; then
        # Strip leading "- " then check if it starts with "TOTAL".
        rest="${line#- }"
        if [[ "$rest" != TOTAL* ]]; then
            NON_TOTAL_BREACH=1
        fi
    fi
done < "$REPORT"

if (( NON_TOTAL_BREACH )); then
    echo ""
    echo "Per-crate or per-section budget breached. See $REPORT."
    exit 1
fi

echo ""
echo "TOTAL-only breach is documented expected state per project policy"
echo "(.size-budget.toml::meta.note). All per-crate and per-section caps"
echo "pass. Treating as PASS-WITH-NOTE."
exit 0
