#!/bin/bash
# Reads the log of a hybrid-laptop boot and says which step of the path is proven.
#   hybrid_verify.sh <logfile>      a saved kernel log (log show --last boot > file)
#   hybrid_verify.sh                reads the live log of this boot and the live sysctls
# NMH_SYSCTL_FILE=<file> supplies sysctls as "name value" lines instead of the live ones (tests).
# Steps, in the order the path takes them (docs/HYBRID-LAPTOPS.md):
#   S1 switch seen by the accelerator      S2 the other GPU's driver published the panel
#   S3 first frame reached the panel       S4 only changed pixels are being stored
#   S5 import ring built (level 2)         S6 first zero-copy flip landed (level 2)
#   R1 the panel was relit after sleep (only when nmhybridresume=1 armed it and the log holds a wake)
set -u
LOG=$(mktemp "${TMPDIR:-/tmp}/nmh.XXXXXX") || exit 2
trap 'rm -f "$LOG"' EXIT
if [ $# -ge 1 ]; then cp "$1" "$LOG" || { echo "cannot read $1" >&2; exit 2; }
else /usr/bin/log show --last boot --style compact --predicate 'eventMessage CONTAINS "hybrid:"' > "$LOG" 2>/dev/null; fi

sc() {   # sysctl value or empty
    if [ -n "${NMH_SYSCTL_FILE:-}" ]; then awk -v k="$1" '$1==k {print $2}' "$NMH_SYSCTL_FILE"
    else /usr/sbin/sysctl -n "$1" 2>/dev/null; fi
}
has() { grep -q -- "$1" "$LOG"; }
pass=0; fail=0; first_missing=""
step() {   # step <id> <ok 0/1> <text> [required 1/0]
    if [ "$2" = 1 ]; then echo "PASS  $1  $3"; pass=$((pass+1))
    elif [ "${4:-1}" = 1 ]; then echo "FAIL  $1  $3"; fail=$((fail+1)); [ -z "$first_missing" ] && first_missing="$1 $3"
    else echo "skip  $1  $3"; fi
}

LEVEL=$(sed -n 's/.*hybrid: switch level \([0-9]\).*/\1/p' "$LOG" | tail -1)
[ -z "$LEVEL" ] && LEVEL=$(sc debug.nmhybrid)
[ -z "$LEVEL" ] && LEVEL=0
if [ "$LEVEL" = 0 ]; then
    echo "hybrid is off: boot-arg nmhybrid is not 1 or 2 (hybrid_switch.sh copy|import, then restart)"
    echo "VERDICT  off"; exit 0
fi

step S1 $(has 'hybrid: switch level' && echo 1 || echo 0) "accelerator saw nmhybrid, level $LEVEL"
step S2 $(has 'hybrid: provider NM' && echo 1 || echo 0) "the other GPU's driver published the panel (line: hybrid: provider ...)"
step S3 $(has 'hybrid: first frame presented by copy' && echo 1 || echo 0) "first composited frame reached the panel by copy"
FR=$(sc debug.nmhybrid_frames); SK=$(sc debug.nmhybrid_words_skipped)
if [ -n "$FR" ] && [ -n "$SK" ]; then step S4 $([ "$FR" -gt 0 ] && [ "$SK" -gt 0 ] && echo 1 || echo 0) "frames $FR, pixels not stored because unchanged $SK"
else step S4 0 "sysctls debug.nmhybrid_frames / _words_skipped not readable" 0; fi

if [ "$LEVEL" = 2 ]; then
    if has 'hybrid: import unavailable'; then
        echo "note  $(grep -- 'hybrid: import unavailable' "$LOG" | tail -1 | sed 's/.*hybrid: //')"
        echo "note  level 2 is not possible on this machine; frames are copied (level 1 steps above)"
        step S5 0 "import ring" 0; step S6 0 "zero-copy flip" 0
    else
        step S5 $(has 'hybrid: import ring' && echo 1 || echo 0) "import ring built in unused graphics-table entries"
        step S6 $(has 'hybrid: zero-copy flip #1' && echo 1 || echo 0) "first zero-copy flip reached the screen"
    fi
    if has 'did not reach the screen' || has 'fallback to copy: a flip never reached the screen'; then
        step S6 0 "a flip never reached the screen: import ended for this boot, copy took over"
    fi
    grep -- 'hybrid: fallback to copy' "$LOG" | sed 's/.*hybrid: /note  /' | sort | uniq -c | sed 's/^ *//' | head -5
fi

# stood down, delayed, or flat-frame notes
grep -- 'hybrid: standing down' "$LOG" | tail -1 | sed 's/.*hybrid: /note  /'
grep -- 'hybrid: waiting [0-9]* s before taking the panel' "$LOG" | tail -1 | sed 's/.*hybrid: /note  /'
LASTSTEP=$(sed -n 's/.*hybrid: step \(.*\)$/\1/p' "$LOG" | tail -1)
[ -n "$LASTSTEP" ] && echo "note  last step the panel driver logged: $LASTSTEP"
# sleep and wake (nmhybridresume=1): only judged when it was armed
if has 'hybrid: resume armed'; then
    if has 'hybrid: resume step:'; then
        step R1 $(has 'hybrid: resume: panel relit\|hybrid: resume: panel was already lit' && echo 1 || echo 0) "after sleep the panel was relit (steps: $(grep -c 'hybrid: resume step:' "$LOG") logged)"
    else
        echo "skip  R1  no sleep and wake in this log"
    fi
fi

if [ "$fail" = 0 ]; then
    if [ "$LEVEL" = 2 ] && has 'hybrid: zero-copy flip #1'; then echo "VERDICT  import proven"
    else echo "VERDICT  copy proven"; fi
    exit 0
fi
echo "VERDICT  not proven: $first_missing"
exit 1
