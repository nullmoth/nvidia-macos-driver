#!/bin/bash
set -u
set -o pipefail
umask 077
# Runs as root. Every binary below is called by absolute path; this keeps that
# safe if a bare call is ever added.
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
HERE=$(cd "$(/usr/bin/dirname "$0")" && pwd -P) || exit 1
[ "$(/usr/bin/id -u)" = 0 ] || { echo "NMCOLLECT_STATUS 77"; exit 0; }
TASK=$(/usr/bin/mktemp -d /private/var/root/1401-operation.XXXXXXXX) || { echo "NMCOLLECT_STATUS 73"; exit 0; }
RETAIN=0
trap '[ "$RETAIN" = 1 ] || /bin/rm -rf "$TASK"' EXIT
/bin/bash "$HERE/nullmoth-setup.sh" "$@" > "$TASK/output.txt" 2>&1
STATUS=$?
EXPORT=0
WARNED="|"
warn() {
    case "$WARNED" in *"|$1|"*) ;; *) printf 'NMCOLLECT_WARNING %s\n' "$1"; WARNED="$WARNED$1|";; esac
    EXPORT=1; RETAIN=1
}
emit() {
    local name=$1 mode=$2 count=$3
    if [ "$mode" = head ]; then
        /usr/bin/head -c "$count" "$TASK/output.txt" > "$TASK/fragment.bin" || { warn read-failed; return 0; }
    else
        /usr/bin/tail -c "$count" "$TASK/output.txt" > "$TASK/fragment.bin" || { warn read-failed; return 0; }
    fi
    if ! /usr/bin/base64 < "$TASK/fragment.bin" | /usr/bin/tr -d '\r\n' > "$TASK/encoded.txt"; then warn encode-failed; return 0; fi
    [ -s "$TASK/encoded.txt" ] || { warn encode-failed; return 0; }
    printf 'NMCOLLECT_FILE %s ' "$name"
    if ! /bin/cat "$TASK/encoded.txt"; then warn encode-failed; return 1; fi
    printf '\n'
}
if SIZE=$(/usr/bin/stat -f %z "$TASK/output.txt") && [[ "$SIZE" =~ ^[0-9]+$ ]]; then
    if [ "$SIZE" -gt 524288 ]; then
        echo "NMCOLLECT_TRUNCATED setup-output.txt"
        emit setup-output-head.txt head 262144
        emit setup-output-tail.txt tail 262144
    elif [ "$SIZE" -gt 0 ]; then emit setup-output.txt tail 524288; fi
else warn stat-failed; fi
if [ "$EXPORT" = 1 ]; then
    [ "$STATUS" != 0 ] || STATUS=74
    printf 'NMCOLLECT_RETAINED %s\n' "$TASK"
else
    if ! /bin/rm -rf "$TASK"; then
        warn cleanup-failed; [ "$STATUS" != 0 ] || STATUS=74
        printf 'NMCOLLECT_RETAINED %s\n' "$TASK"
    fi
fi
printf 'NMCOLLECT_STATUS %s\n' "$STATUS"
