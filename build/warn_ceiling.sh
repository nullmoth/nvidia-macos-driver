#!/bin/bash
# warn_ceiling.sh <component> <warning count> [log]: stops a build whose compiler warnings rose above the committed ceiling
# for <component> in build/warnings.txt ("<component> <ceiling>" per line). A component with no line has a ceiling of 0.
# Lower a ceiling when warnings are fixed; never raise one to let a new warning through.
set -u
NAME=${1:?component}; N=${2:?warning count}; LOG=${3:-}
HERE=$(cd "$(dirname "$0")" && pwd)
MAX=$(awk -v n="$NAME" '$1 == n { print $2; exit }' "$HERE/warnings.txt" 2>/dev/null)
MAX=${MAX:-0}
case "$N" in ''|*[!0-9]*) echo "STOP: $NAME: warning count '$N' is not a number"; exit 4 ;; esac
if [ "$N" -gt "$MAX" ]; then
    echo "STOP: $NAME has $N compiler warnings, more than its ceiling of $MAX in build/warnings.txt"
    [ -n "$LOG" ] && [ -f "$LOG" ] && grep 'warning:' "$LOG" | head -20
    exit 4
fi
echo "$NAME warnings $N (ceiling $MAX)"
