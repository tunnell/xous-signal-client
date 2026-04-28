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
#  3. Locates signal-cli's account.db for the sender account (the one
#     signal-cli is linked to as a secondary device).
#  4. Deletes any rows in `session` whose address column matches the
#     emulator's account UUID (XSC_EMULATOR_UUID), forcing signal-cli's
#     next outbound to issue a PreKey-bundle envelope.
#  5. Runs scan-receive.sh once to re-establish a clean session and
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
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

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

# 3. Locate signal-cli's account.db for the sender account.
SIGNAL_CLI_ROOT="${SIGNAL_CLI_ROOT:-$HOME/.local/share/signal-cli}"
# signal-cli stores accounts under data/<account_id>.d/account.db.
# accounts.json maps phone numbers to account IDs; we walk it to find
# the right one rather than guessing.
ACCOUNTS_JSON="$SIGNAL_CLI_ROOT/data/accounts.json"
if [[ ! -f "$ACCOUNTS_JSON" ]]; then
    echo "ERROR: signal-cli accounts.json not found: $ACCOUNTS_JSON" >&2
    exit 2
fi

SIGNAL_DB="$(python3 -c "
import json, os, sys
target = sys.argv[1]
with open(sys.argv[2]) as f:
    data = json.load(f)
for acc in data.get('accounts', []):
    if acc.get('number') == target:
        path = acc.get('path')
        if path and not os.path.isabs(path):
            path = os.path.join(os.path.dirname(sys.argv[2]), path)
        # signal-cli convention: \"<path>\" is a file marker, the DB
        # lives in \"<path>.d/account.db\".
        db_dir = path + '.d' if not path.endswith('.d') else path
        print(os.path.join(db_dir, 'account.db'))
        sys.exit(0)
sys.exit(3)
" "$XSC_RECIPIENT_NUMBER" "$ACCOUNTS_JSON" 2>/dev/null || true)"

if [[ -z "$SIGNAL_DB" || ! -f "$SIGNAL_DB" ]]; then
    echo "ERROR: signal-cli account.db not found for $XSC_RECIPIENT_NUMBER" >&2
    echo "       walked $ACCOUNTS_JSON; got: '${SIGNAL_DB:-<empty>}'" >&2
    exit 2
fi

# 4. Delete sessions for the emulator UUID. This is the documented
# B2-sibling workaround. See bug-arcs/b005 section 2026-04-28.
# Look up the UUID via signal-cli's recipient table (JOIN by phone
# number) so the script works with just XSC_SENDER_NUMBER set.
echo "Clearing signal-cli sessions for emulator account ($XSC_SENDER_NUMBER)..."
read -r EMULATOR_UUID DELETED < <(python3 -c "
import sqlite3, sys
db, number = sys.argv[1], sys.argv[2]
con = sqlite3.connect(db)
row = con.execute('SELECT aci FROM recipient WHERE number = ?', (number,)).fetchone()
if not row or not row[0]:
    print('NONE 0')
    sys.exit(0)
uuid = row[0]
cur = con.execute('DELETE FROM session WHERE address = ?', (uuid,))
con.commit()
print(uuid, cur.rowcount)
con.close()
" "$SIGNAL_DB" "$XSC_SENDER_NUMBER")

if [[ "$EMULATOR_UUID" == "NONE" ]]; then
    echo "  WARN: no recipient row for $XSC_SENDER_NUMBER in signal-cli" >&2
    echo "        recipient table; nothing to clear. signal-cli may not" >&2
    echo "        have ever messaged this number, in which case there is" >&2
    echo "        no stale session to worry about. Continuing." >&2
else
    echo "  uuid=$EMULATOR_UUID; deleted $DELETED row(s) from session table"
fi

# 5. Warm up: run scan-receive.sh once to re-establish a clean session.
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
