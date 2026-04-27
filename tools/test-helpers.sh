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
