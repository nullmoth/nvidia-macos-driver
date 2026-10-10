#!/bin/bash
set -u
set -o pipefail
umask 077
HERE=$(cd "$(/usr/bin/dirname "$0")" && pwd -P) || exit 1
[ "$(/usr/bin/id -u)" = 0 ] || { echo "NMCOLLECT_STATUS 77"; exit 0; }
TASK=$(/usr/bin/mktemp -d /private/var/root/1401-logs.XXXXXXXX) || { echo "NMCOLLECT_STATUS 73"; exit 0; }
RETAIN=0
trap '[ "$RETAIN" = 1 ] || /bin/rm -rf "$TASK"' EXIT
"$HERE/nullmoth-log-watch" "$HERE/nullmoth-setup.sh" "$TASK/files" > "$TASK/collect.txt" 2>&1
STATUS=$?
EXPORT=0
WARNED="|"
warn() {
    case "$WARNED" in *"|$1|"*) ;; *) printf 'NMCOLLECT_WARNING %s\n' "$1"; WARNED="$WARNED$1|";; esac
    EXPORT=1; RETAIN=1
}
if ! /bin/mkdir -p "$TASK/files" || ! /bin/cp "$TASK/collect.txt" "$TASK/files/collect.txt"; then warn copy-failed; fi
COUNT=0
LIMIT_REPORTED=0
emit() {
    local file=$1 name size
    [ -e "$file" ] || return 0
    [ -f "$file" ] && [ ! -L "$file" ] || { warn invalid-file; return 0; }
    name=${file##*/}
    [[ "$name" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,199}$ ]] || { warn invalid-file; return 0; }
    [ "$COUNT" -lt 48 ] || {
        if [ "$LIMIT_REPORTED" = 0 ]; then echo "NMCOLLECT_TRUNCATED file-count"; LIMIT_REPORTED=1; fi
        return 0
    }
    size=$(/usr/bin/stat -f %z "$file") || { warn stat-failed; return 0; }
    [[ "$size" =~ ^[0-9]+$ ]] || { warn stat-failed; return 0; }
    [ "$size" -gt 0 ] || return 0
    if [ "$size" -gt 524288 ]; then echo "NMCOLLECT_TRUNCATED $name"; fi
    if ! /usr/bin/tail -c 524288 "$file" > "$TASK/fragment.bin"; then warn read-failed; return 0; fi
    if ! /usr/bin/base64 < "$TASK/fragment.bin" | /usr/bin/tr -d '\r\n' > "$TASK/encoded.txt"; then warn encode-failed; return 0; fi
    [ -s "$TASK/encoded.txt" ] || { warn encode-failed; return 0; }
    printf 'NMCOLLECT_FILE %s ' "$name"
    if ! /bin/cat "$TASK/encoded.txt"; then warn encode-failed; return 1; fi
    printf '\n'
    COUNT=$((COUNT + 1))
}
for name in hardware-map.json driver-state.txt driver-kernel-log.txt driver-display.txt previous-boot-kernel-log.txt driver-plugin-log.txt driver-crash-window.txt collect.txt driver-update-log.txt; do emit "$TASK/files/$name" || break; done
if /usr/bin/find "$TASK/files" -maxdepth 1 -type f -print | /usr/bin/sort > "$TASK/file-list.txt"; then
    while IFS= read -r file; do
        case "${file##*/}" in hardware-map.json|driver-state.txt|driver-kernel-log.txt|driver-display.txt|previous-boot-kernel-log.txt|driver-plugin-log.txt|driver-crash-window.txt|collect.txt|driver-update-log.txt) continue;; esac
        emit "$file" || break
    done < "$TASK/file-list.txt"
else warn read-failed; fi
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
