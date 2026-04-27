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
