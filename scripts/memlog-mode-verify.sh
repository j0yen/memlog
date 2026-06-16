#!/usr/bin/env bash
# memlog-mode-verify.sh — verify /dev/memlog is writable with correct mode and do a real round-trip
set -euo pipefail

DEVICE=/dev/memlog
REQUIRED_MODE=660
REQUIRED_GROUP=memlog

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# AC(a): /dev/memlog exists and mode is 0660 with group memlog
if [[ ! -e "$DEVICE" ]]; then
    fail "/dev/memlog does not exist"
fi

read -r actual_mode actual_group < <(stat -c '%a %G' "$DEVICE")

if [[ "$actual_mode" != "$REQUIRED_MODE" ]]; then
    fail "/dev/memlog mode is $actual_mode, expected $REQUIRED_MODE (run memlog-mode-repair.sh install)"
fi

if [[ "$actual_group" != "$REQUIRED_GROUP" ]]; then
    fail "/dev/memlog group is $actual_group, expected $REQUIRED_GROUP"
fi

echo "OK: /dev/memlog mode=${actual_mode} group=${actual_group}"

# AC(b): current user is in the memlog group
if ! id -Gn | grep -qw "$REQUIRED_GROUP"; then
    fail "current user ($(id -un)) is not in the '$REQUIRED_GROUP' group — run: sudo usermod -aG $REQUIRED_GROUP $(id -un)"
fi

echo "OK: user $(id -un) is in group $REQUIRED_GROUP"

# AC(c): real round-trip — write a test record and verify total_writes increments by 1
before_writes=$(memlog stats 2>&1 | awk '/^total_writes/ {print $2}')
if [[ -z "$before_writes" ]]; then
    fail "could not read total_writes from 'memlog stats'"
fi

test_data="memlog-mode-verify-test-$(date -u +%s)"
# memlog write reads raw bytes from stdin (CBOR blob, but any bytes work for a write test)
printf '%s' "$test_data" | memlog write 2>&1 || fail "memlog write failed (device writable but CLI failed?)"

after_writes=$(memlog stats 2>&1 | awk '/^total_writes/ {print $2}')
if [[ -z "$after_writes" ]]; then
    fail "could not read total_writes after write"
fi

expected=$((before_writes + 1))
if [[ "$after_writes" -ne "$expected" ]]; then
    fail "total_writes did not increment by 1: before=$before_writes after=$after_writes expected=$expected"
fi

echo "OK: round-trip write succeeded — total_writes $before_writes → $after_writes"
echo "PASS: /dev/memlog is correctly configured and writable"
