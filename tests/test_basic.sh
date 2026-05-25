#!/usr/bin/env bash
# Basic memlog functional test. Runs after the module is insmodded.
# Exit non-zero on any failure; prints PASS/FAIL per assertion.
set -u
ME="${BASH_SOURCE[0]##*/}"
CLI="$(realpath "$(dirname "$0")/../cli/memlog")"
FAIL=0

say() { printf '%-50s %s\n' "$1" "$2"; }
ok()   { say "$1" "PASS"; }
no()   { say "$1" "FAIL: $2"; FAIL=1; }

if [[ ! -c /dev/memlog ]]; then
    echo "$ME: /dev/memlog not present — load the module first (modprobe memlog)"
    exit 2
fi
if [[ ! -r /dev/memlog ]]; then
    echo "$ME: /dev/memlog not readable as $(id -un); add yourself to the memlog group or run as root"
    exit 2
fi
if ! command -v python3 >/dev/null; then
    echo "$ME: python3 required"; exit 2
fi

# 1. version ioctl reports schema 1
v=$("$CLI" version 2>&1 | awk '{print $NF}')
[[ "$v" == "1" ]] && ok "ioctl GET_VERSION returns 1" || no "ioctl GET_VERSION" "got '$v'"

# 2. stats works
"$CLI" stats >/dev/null 2>&1 && ok "ioctl STATS works" || no "ioctl STATS" "exit $?"

# 3. write a small payload
payload=$(printf 'hello-memlog-%s' "$(date +%s.%N)")
echo -n "$payload" | "$CLI" write >/dev/null 2>&1
if [[ $? -eq 0 ]]; then
    ok "write small payload"
else
    no "write small payload" "exit $?"
fi

# 4. read it back via show (raw)
got=$("$CLI" show --limit 1 2>&1 | tail -1)
if [[ "$got" == "$payload" ]]; then
    ok "read echoes payload"
else
    no "read echoes payload" "got '$got' want '$payload'"
fi

# 5. stats reports records_in_ring >= 1, total_writes >= 1
records=$("$CLI" stats 2>&1 | awk '/records_in_ring/{print $2}')
[[ "$records" =~ ^[0-9]+$ && "$records" -ge 1 ]] && ok "stats: records_in_ring >= 1" \
    || no "stats records_in_ring" "got '$records'"
writes=$("$CLI" stats 2>&1 | awk '/total_writes/{print $2}')
[[ "$writes" =~ ^[0-9]+$ && "$writes" -ge 1 ]] && ok "stats: total_writes >= 1" \
    || no "stats total_writes" "got '$writes'"

# 6. payload > MEMLOG_RECORD_MAX gets -EMSGSIZE
head -c $((65 * 1024)) /dev/urandom | "$CLI" write 2>/dev/null
if [[ $? -ne 0 ]]; then
    ok "oversized payload rejected"
else
    no "oversized payload" "should have failed"
fi

# 7. uid filter — without admin, filtering to a foreign uid returns -EPERM
foreign_uid=$((UID + 1))
if [[ $EUID -ne 0 ]]; then
    err=$("$CLI" show --uid "$foreign_uid" --limit 1 2>&1 || true)
    if echo "$err" | grep -qi "permission denied\|Operation not permitted\|errno"; then
        ok "filter to foreign uid blocked"
    else
        # Note: Python's fcntl raises PermissionError or OSError(EPERM)
        # — accept any error
        if "$CLI" show --uid "$foreign_uid" --limit 1 >/dev/null 2>&1; then
            no "filter to foreign uid" "should have blocked"
        else
            ok "filter to foreign uid blocked"
        fi
    fi
else
    say "filter to foreign uid blocked" "SKIP (running as root)"
fi

if (( FAIL )); then
    echo
    echo "$ME: FAILED"; exit 1
fi
echo
echo "$ME: OK"
