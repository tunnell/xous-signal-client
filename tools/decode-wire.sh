#!/usr/bin/env bash
# tools/decode-wire.sh
#
# Decodes Content protobufs captured during a scan-send.sh run via the
# XSCDEBUG_DUMP env var, and verifies field tags match canonical
# SignalService.proto. The dump file format is one labelled hex line
# per artifact, e.g.:
#
#   [<ts>] Content protobuf (DataMessage, ...) (len=22): 0a14...
#   [<ts>] Padded plaintext (...) (len=160): 0a14...80000...
#   [<ts>] Ciphertext (envelope type=1) for <uuid>/<dev> (len=233): 4408...
#   [<ts>] Content protobuf (SyncMessage::Sent, ...) (len=71): 1245...
#
# This script:
#   - Decodes every Content protobuf line via `protoc --decode_raw`.
#   - Verifies DataMessage has tag 1 (body) and tag 7 (timestamp). The
#     missing-tag-7 bug from v6 of the Phase A arc would be caught
#     here.
#   - Verifies SyncMessage.Sent (when present) has tag 2 (timestamp),
#     tag 3 (inner DataMessage), tag 7 (destinationServiceId).
#   - Reports the timestamp value(s) seen across all locations and
#     flags inconsistencies.
#
# Prerequisites:
#   - protoc on PATH (apt: protobuf-compiler)
#   - xxd on PATH
#   - A wire dump file (default /tmp/xsc-wire-dump.txt)
#
# Output:
#   - Per-protobuf decoded structure on stdout
#   - Verification summary at the end
#
# Exit codes:
#   0 = all Content protobufs parsed and required tags present
#   1 = at least one Content protobuf failed verification
#   2 = setup failure (missing tools, missing dump file)
#
# Usage:
#   ./tools/decode-wire.sh
#   ./tools/decode-wire.sh /path/to/xsc-wire-dump.txt

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=test-helpers.sh
source "$SCRIPT_DIR/test-helpers.sh"

DUMP_FILE="${1:-/tmp/xsc-wire-dump.txt}"

xsc_require_cmd protoc "apt install protobuf-compiler" || exit 2
xsc_require_cmd xxd "apt install xxd" || exit 2

if [[ ! -f "$DUMP_FILE" ]]; then
    echo "Wire dump not found: $DUMP_FILE" >&2
    echo "Run a scan with XSCDEBUG_DUMP=1 first (./tools/scan-send.sh)." >&2
    exit 2
fi

decode_hex() {
    local hex="$1"
    echo "$hex" | xxd -r -p | protoc --decode_raw
}

# Parse the dump line by line, decode Content lines.
DM_COUNT=0
SM_COUNT=0
ALL_TS=()
FAIL=0

while IFS= read -r line; do
    [[ -z "$line" ]] && continue

    # Match Content lines: "Content protobuf (...) (len=N): HEX"
    if [[ "$line" =~ Content\ protobuf\ \(([^,]+),.*\(len=([0-9]+)\):\ ([0-9a-fA-F]+)$ ]]; then
        kind="${BASH_REMATCH[1]}"
        hex="${BASH_REMATCH[3]}"
        echo "================================================"
        echo "Content: $kind"
        echo "================================================"
        decoded="$(decode_hex "$hex" 2>&1)" || {
            echo "DECODE FAILED" >&2
            echo "$decoded"
            FAIL=1
            continue
        }
        echo "$decoded"

        if [[ "$kind" == *DataMessage* ]]; then
            DM_COUNT=$((DM_COUNT + 1))
            # Verify body (tag 1) and timestamp (tag 7) present at top
            # level inside the dataMessage submessage (tag 1 of Content).
            if ! grep -E "^\s*1: \"" <<<"$decoded" >/dev/null; then
                echo "  WARN: DataMessage.body (tag 1) absent" >&2
                FAIL=1
            fi
            if ! grep -E "^\s*7: [0-9]+" <<<"$decoded" >/dev/null; then
                echo "  FAIL: DataMessage.timestamp (tag 7) absent — would be the v6 bug" >&2
                FAIL=1
            fi
            # Capture the tag-7 timestamp value (skip the leading "7:"
            # field-number prefix; the value is the second token).
            ts="$(grep -E "^\s*7: [0-9]+$" <<<"$decoded" | head -1 | awk '{print $2}')"
            [[ -n "$ts" ]] && ALL_TS+=("dm:$ts")
        elif [[ "$kind" == *SyncMessage* ]]; then
            SM_COUNT=$((SM_COUNT + 1))
            # Verify SyncMessage.sent.timestamp (tag 2 inside tag 1
            # inside Content tag 2) and inner DataMessage tag 3.
            if ! grep -E "^\s*2: [0-9]+" <<<"$decoded" >/dev/null; then
                echo "  FAIL: Sent.timestamp (tag 2) absent" >&2
                FAIL=1
            fi
            if ! grep -E "^\s*3 \{" <<<"$decoded" >/dev/null; then
                echo "  FAIL: Sent.message (tag 3) absent" >&2
                FAIL=1
            fi
            if ! grep -E "^\s*7: \"" <<<"$decoded" >/dev/null; then
                echo "  WARN: Sent.destinationServiceId (tag 7) absent" >&2
            fi
            # Capture timestamps from this sync block: tag 2 (Sent.timestamp)
            # at the SyncMessage level and tag 7 (inner DataMessage.timestamp).
            # awk extracts the value, skipping the field-number prefix.
            while IFS= read -r ts; do
                [[ -n "$ts" ]] && ALL_TS+=("sm:$ts")
            done < <(grep -E "^\s*[27]: [0-9]+$" <<<"$decoded" | awk '{print $2}')
        fi
        echo ""
    fi
done < "$DUMP_FILE"

echo "================================================"
echo "Verification summary"
echo "================================================"
echo "  DataMessage Content protobufs: $DM_COUNT"
echo "  SyncMessage Content protobufs: $SM_COUNT"

# Timestamp consistency: collect distinct values across all observed
# tag-7 (DataMessage) and tag-2 (SyncMessage.Sent) positions. They
# should match for a single send.
declare -A SEEN
for entry in "${ALL_TS[@]:-}"; do
    val="${entry#*:}"
    SEEN[$val]=1
done
distinct_count=${#SEEN[@]}
echo "  Distinct timestamp values: $distinct_count"
for ts in "${!SEEN[@]}"; do
    echo "    $ts"
done

if (( DM_COUNT == 0 )); then
    echo "  FAIL: no DataMessage Content protobufs decoded" >&2
    FAIL=1
fi

if (( distinct_count > 1 && SM_COUNT > 0 )); then
    echo "  WARN: multiple distinct timestamps across DataMessage + SyncMessage." >&2
    echo "    A single send should reuse one timestamp across all five wire" >&2
    echo "    locations. Investigate." >&2
fi

if (( FAIL )); then
    echo ""
    echo "RESULT: FAIL"
    exit 1
fi

echo ""
echo "RESULT: PASS"
exit 0
