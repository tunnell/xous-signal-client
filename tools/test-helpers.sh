# tools/test-helpers.sh
#
# Shared shell library. Source from other tools/*.sh scripts.
# Do not run directly.

# Exit if the user has sourced this file as a top-level script.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    echo "test-helpers.sh is a library; source it from another script." >&2
    exit 64
fi

# Resolve the repository root from this file's location, regardless of
# the caller's working directory.
xsc_repo_root() {
    local here
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    cd "$here/.." && pwd
}

# Load tools/.env if present. Missing file is not fatal here — callers
# decide whether they need it.
xsc_load_env() {
    local root env
    root="$(xsc_repo_root)"
    env="$root/tools/.env"
    if [[ -f "$env" ]]; then
        # shellcheck source=/dev/null
        source "$env"
        return 0
    fi
    return 1
}

# Require a set of env-var names to be non-empty. Prints which ones are
# missing and returns 2 (config error) if any are absent.
xsc_require_env() {
    local missing=()
    local v
    for v in "$@"; do
        if [[ -z "${!v:-}" ]]; then
            missing+=("$v")
        fi
    done
    if (( ${#missing[@]} > 0 )); then
        echo "Missing required env vars: ${missing[*]}" >&2
        echo "Configure them in tools/.env (see tools/test-env.example)." >&2
        return 2
    fi
    return 0
}

# Require a command to be on PATH. Returns 2 if not found.
xsc_require_cmd() {
    local cmd="$1"
    local hint="${2:-}"
    if ! command -v "$cmd" &>/dev/null; then
        echo "Required command not found: $cmd" >&2
        if [[ -n "$hint" ]]; then
            echo "  $hint" >&2
        fi
        return 2
    fi
    return 0
}

# Prime the recipient session by sending an inbound message from
# signal-cli. This forces session establishment if absent.
# Args: recipient_account, sender_account, [body]
xsc_prime_session() {
    local recipient="$1"
    local sender="$2"
    local body="${3:-Test priming}"
    signal-cli -a "$recipient" send -m "$body" "$sender"
}

# Run signal-cli receive on an account and grep for an expected body.
# Returns 0 on match, 1 on no match.
# Args: account, expected_body, [timeout_seconds]
xsc_verify_receipt() {
    local account="$1"
    local expected="$2"
    local timeout="${3:-30}"
    timeout "$timeout" signal-cli -a "$account" receive 2>&1 | \
        grep -q "Body: $expected"
}

# Bytes -> human-readable size (KiB / MiB).
xsc_fmt_bytes() {
    local b="$1"
    awk -v b="$b" 'BEGIN {
        if (b < 1024) { printf "%d B\n", b }
        else if (b < 1024 * 1024) { printf "%.2f KiB\n", b / 1024 }
        else { printf "%.2f MiB\n", b / 1024 / 1024 }
    }'
}

# Clear signal-cli's stored sessions for a target UUID, looked up by
# phone number on the named sender account. Forces signal-cli's next
# outbound to that target to issue a PreKey-bundle (envelope type 3)
# instead of reusing a stored session and sending a SignalMessage
# (envelope type 1).
#
# This is the B2-sibling priming-flake mitigation. When the emulator's
# PDDB is restored to a snapshot but signal-cli's session table for
# the emulator's UUID has advanced past the snapshot, signal-cli sends
# a SignalMessage that the rolled-back emulator cannot decrypt.
# Clearing here forces a fresh PreKey-bundle session establishment,
# which the rolled-back emulator can pick up cleanly.
#
# Args: signal_cli_sender_account_e164, target_number_e164
# Returns:
#   0 = sessions cleared (or nothing to clear; both are success)
#   2 = setup error (accounts.json missing, python3 missing, etc.)
xsc_clear_signal_cli_sessions() {
    local sender="$1"
    local target="$2"
    local signal_cli_root="${SIGNAL_CLI_ROOT:-$HOME/.local/share/signal-cli}"
    local accounts_json="$signal_cli_root/data/accounts.json"

    if [[ ! -f "$accounts_json" ]]; then
        echo "WARN: signal-cli accounts.json not found: $accounts_json" >&2
        echo "      Skipping session-clear; priming may flake (issue #9)." >&2
        return 2
    fi
    if ! command -v python3 &>/dev/null; then
        echo "WARN: python3 not found; cannot clear signal-cli sessions" >&2
        echo "      Skipping session-clear; priming may flake (issue #9)." >&2
        return 2
    fi

    python3 - "$accounts_json" "$sender" "$target" <<'PYEOF'
import sqlite3, json, os, sys

accounts_json, sender, target = sys.argv[1], sys.argv[2], sys.argv[3]

# Locate sender's account.db.
with open(accounts_json) as f:
    data = json.load(f)
sender_path = None
for acc in data.get("accounts", []):
    if acc.get("number") == sender:
        sender_path = acc.get("path")
        break
if not sender_path:
    print(f"  signal-cli has no account for {sender}; nothing to clear")
    sys.exit(0)

if not os.path.isabs(sender_path):
    sender_path = os.path.join(os.path.dirname(accounts_json), sender_path)
db_dir = sender_path if sender_path.endswith(".d") else sender_path + ".d"
db_path = os.path.join(db_dir, "account.db")
if not os.path.exists(db_path):
    print(f"  signal-cli db not at {db_path}; nothing to clear")
    sys.exit(0)

# Look up target UUID and delete any session rows.
con = sqlite3.connect(db_path)
row = con.execute("SELECT aci FROM recipient WHERE number = ?", (target,)).fetchone()
if not row or not row[0]:
    print(f"  signal-cli has no recipient row for {target}; nothing to clear")
    con.close()
    sys.exit(0)
uuid = row[0]
cur = con.execute("DELETE FROM session WHERE address = ?", (uuid,))
con.commit()
print(f"  cleared {cur.rowcount} session row(s) for {target} (uuid={uuid})")
con.close()
PYEOF
    return 0
}

# Verify signal-cli is linked to a given account with at least one
# expected linked secondary. Per the canonical topology in
# ~/workdir/ACCOUNT-MAPPING.md, the test harness REQUIRES `signal-cli-test`
# (or whatever the local signal-cli installation registered itself as)
# to be present as a linked secondary on the verifying / sending
# account. Per the hard rule in Phase R+: refuse to run the scan if
# the expected secondary is absent.
#
# Args: account_e164, expected_device_name_substring [, ...]
# Returns:
#   0 = signal-cli sees the account AND at least one of the expected
#       secondaries is in listDevices output
#   2 = signal-cli doesn't have this account, or no expected secondary
#       found — caller should exit 2 (setup error)
xsc_verify_linked_device() {
    local account="$1"; shift
    if (( $# == 0 )); then
        echo "xsc_verify_linked_device: caller must list at least one expected device name" >&2
        return 64
    fi
    local devices
    devices="$(signal-cli -a "$account" listDevices 2>&1)"
    local rc=$?
    if (( rc != 0 )); then
        echo "signal-cli listDevices failed for $account (rc=$rc):" >&2
        echo "$devices" | head -5 >&2
        return 2
    fi
    local expected
    for expected in "$@"; do
        if grep -q "Name: $expected" <<<"$devices" || \
           grep -qE "Name:.*$expected" <<<"$devices"; then
            return 0
        fi
    done
    echo "Expected linked device(s) not found on $account:" >&2
    printf "  - %s\n" "$@" >&2
    echo "Actual listDevices output:" >&2
    echo "$devices" | sed 's/^/  /' >&2
    return 2
}
