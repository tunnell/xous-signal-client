#!/usr/bin/env bash
# tools/demo-prep.sh
#
# Recording-day setup for the hosted-mode demo. Mitigates the
# B2-sibling priming flake (`InvalidMessage(Whisper, "decryption
# failed")`) documented in `bug-arcs/b005-signal-cli-libsignal-decrypt.md`
# section 2026-04-28: when the emulator's PDDB is restored to a
# snapshot but signal-cli's `session` table still holds a session for
# the emulator UUID, signal-cli sends Whisper SignalMessage instead of
# PreKeyMessage and the rolled-back emulator cannot decrypt.
#
# What it does, in order:
#  1. Loads tools/.env so XSC_RECIPIENT_NUMBER / XSC_SENDER_NUMBER /
#     XSC_DEMO_PEER_UUID are visible.
#  2. Restores the emulator's PDDB snapshot to a known-good linked state.
#  3. Verifies signal-cli is set up (accounts.json present).
#  4. Looks up the emulator's UUID for the "To record:" hint at the end.
#  5. Clears signal-cli sessions for the emulator UUID, via the shared
#     xsc_clear_signal_cli_sessions helper in test-helpers.sh. This is
#     the documented B2-sibling priming-flake mitigation (issue #9):
#     forces signal-cli's next outbound to issue a PreKey-bundle
#     envelope instead of a SignalMessage.
#  6. Runs scan-receive.sh once to re-establish a clean session and
#     warm-up the emulator's WS auth + receive worker.
#
# After this script exits 0, hosted mode can be launched with
# XSC_DEMO_PEER_UUID set and the recording can begin.
#
# Usage:
#   source tools/.env  # if not already sourced
#   ./tools/demo-prep.sh
#
# Exit codes:
#   0 = ready for recording
#   1 = setup failure (missing env, missing files, scan-receive
#       failed); actionable error printed to stderr
#   2 = invariant violation (e.g. account.db not where expected) —
#       indicates the demo prep needs a maintainer pass

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=test-helpers.sh
source "$SCRIPT_DIR/test-helpers.sh"
ROOT="$(xsc_repo_root)"

# 1. Load tools/.env if present and unsourced.
if [[ -f "$ROOT/tools/.env" && -z "${XSC_RECIPIENT_NUMBER:-}" ]]; then
    # shellcheck source=.env
    source "$ROOT/tools/.env"
fi

if [[ -z "${XSC_RECIPIENT_NUMBER:-}" ]]; then
    echo "ERROR: XSC_RECIPIENT_NUMBER not set; load tools/.env first" >&2
    echo "  Example: source tools/.env" >&2
    exit 1
fi

if [[ -z "${XSC_DEMO_PEER_UUID:-}" ]]; then
    echo "WARN: XSC_DEMO_PEER_UUID not set; the demo will rely on the" >&2
    echo "      V1 most-recent-sender mechanism instead of pre-seeding." >&2
fi

if [[ -z "${XSC_SENDER_NUMBER:-}" ]]; then
    echo "ERROR: XSC_SENDER_NUMBER not set; cannot identify emulator account" >&2
    exit 1
fi

# 2. Restore PDDB snapshot.
XOUS_CORE_PATH="${XOUS_CORE_PATH:-$ROOT/../xous-core}"
PDDB_SNAPSHOT="${XSC_PDDB_IMAGE:-$XOUS_CORE_PATH/tools/pddb-images/hosted-linked-display-verified.bin}"
HOSTED_BIN="$XOUS_CORE_PATH/tools/pddb-images/hosted.bin"

if [[ ! -f "$PDDB_SNAPSHOT" ]]; then
    echo "ERROR: PDDB snapshot not found: $PDDB_SNAPSHOT" >&2
    exit 1
fi

echo "Restoring PDDB snapshot..."
cp "$PDDB_SNAPSHOT" "$HOSTED_BIN"

# 3. Verify signal-cli is set up; demo-prep needs it for both the
# session-clear (next step) and the scan-receive.sh warm-up.
SIGNAL_CLI_ROOT="${SIGNAL_CLI_ROOT:-$HOME/.local/share/signal-cli}"
ACCOUNTS_JSON="$SIGNAL_CLI_ROOT/data/accounts.json"
if [[ ! -f "$ACCOUNTS_JSON" ]]; then
    echo "ERROR: signal-cli accounts.json not found: $ACCOUNTS_JSON" >&2
    echo "       Demo prep needs signal-cli installed and linked." >&2
    exit 2
fi

# 4. Look up the emulator's UUID for the "To record:" hint at the end.
# Independent of step 5's session-clear; used only to print a helpful
# XSC_DEMO_PEER_UUID line if available.
EMULATOR_UUID="$(python3 - "$ACCOUNTS_JSON" "$XSC_RECIPIENT_NUMBER" "$XSC_SENDER_NUMBER" <<'PYEOF' 2>/dev/null || true
import sqlite3, json, os, sys
accounts_json, sender, target = sys.argv[1], sys.argv[2], sys.argv[3]
with open(accounts_json) as f:
    data = json.load(f)
sender_path = next((a.get("path") for a in data.get("accounts", []) if a.get("number") == sender), None)
if not sender_path:
    sys.exit(0)
if not os.path.isabs(sender_path):
    sender_path = os.path.join(os.path.dirname(accounts_json), sender_path)
db_dir = sender_path if sender_path.endswith(".d") else sender_path + ".d"
db_path = os.path.join(db_dir, "account.db")
if not os.path.exists(db_path):
    sys.exit(0)
con = sqlite3.connect(db_path)
row = con.execute("SELECT aci FROM recipient WHERE number = ?", (target,)).fetchone()
if row and row[0]:
    print(row[0])
con.close()
PYEOF
)"

# 5. Clear signal-cli sessions for the emulator UUID (the documented
# B2-sibling workaround — see bug-arcs/b005 section 2026-04-28 and
# issue #9). Forces signal-cli to issue a PreKey-bundle on next send
# instead of reusing a stale session that the rolled-back PDDB cannot
# decrypt. The same helper is used by tools/scan-send.sh and
# tools/scan-receive.sh.
echo "Clearing signal-cli sessions for emulator account ($XSC_SENDER_NUMBER)..."
xsc_clear_signal_cli_sessions "$XSC_RECIPIENT_NUMBER" "$XSC_SENDER_NUMBER" || true

# 6. Warm up: run scan-receive.sh once to re-establish a clean session.
# scan-receive.sh handles its own boot/teardown of the emulator.
echo ""
echo "=== Warming up via scan-receive.sh ==="
if "$SCRIPT_DIR/scan-receive.sh"; then
    echo ""
    echo "Demo prep complete. Hosted mode is ready for recording."
    echo "To record:"
    if [[ -n "${EMULATOR_UUID:-}" && "$EMULATOR_UUID" != "NONE" ]]; then
        echo "  export XSC_DEMO_PEER_UUID=\"$EMULATOR_UUID\"   # or whatever your peer is"
    else
        echo "  export XSC_DEMO_PEER_UUID=<uuid>   # set the demo peer's UUID here"
    fi
    echo "  cd $XOUS_CORE_PATH && cargo xtask run sigchat:$ROOT/target/release/xous-signal-client"
    exit 0
else
    rc=$?
    echo "" >&2
    echo "ERROR: scan-receive.sh failed (exit $rc); demo prep incomplete." >&2
    echo "       See scan-receive log for diagnostic details." >&2
    exit 1
fi
