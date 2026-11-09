#!/bin/bash
# The one switch for hybrid laptops: boot-arg nmhybrid.
#   hybrid_switch.sh status          what is set now
#   hybrid_switch.sh copy            nmhybrid=1  frames are copied into the other GPU's panel, changed pixels only
#   hybrid_switch.sh import          nmhybrid=2  Intel 12th/13th gen panels are scanned out in place; the rest copy
#   hybrid_switch.sh off             remove nmhybrid (the default)
# Other boot-args are kept. Writing needs root. NMH_NVRAM overrides the nvram tool and --dry-run prints the new
# boot-args without writing them (both used by tools/test_hybrid_switch.sh).
set -u
NVRAM=${NMH_NVRAM:-/usr/sbin/nvram}
DRY=0; CMD=""
for a in "$@"; do case "$a" in --dry-run) DRY=1;; *) CMD=$a;; esac; done

current() { "$NVRAM" boot-args 2>/dev/null | sed -n 's/^boot-args[[:space:]]*//p'; }

# every boot-arg except nmhybrid, then the new one
compose() {
    local old=$1 new=$2 out="" w
    for w in $old; do
        case "$w" in nmhybrid=*) ;; *) out="$out${out:+ }$w";; esac
    done
    [ -n "$new" ] && out="$out${out:+ }nmhybrid=$new"
    printf '%s' "$out"
}

OLD=$(current)
case "$CMD" in
status)
    v=""; for w in $OLD; do case "$w" in nmhybrid=*) v=${w#nmhybrid=};; esac; done
    case "$v" in
        "") echo "hybrid: off (nmhybrid not set)";;
        1)  echo "hybrid: level 1, copy (nmhybrid=1)";;
        2)  echo "hybrid: level 2, import (nmhybrid=2)";;
        *)  echo "hybrid: off (nmhybrid=$v is not 1 or 2)";;
    esac
    exit 0;;
copy)   NEW=$(compose "$OLD" 1);;
import) NEW=$(compose "$OLD" 2);;
off)    NEW=$(compose "$OLD" "");;
*) echo "usage: $0 status | copy | import | off [--dry-run]" >&2; exit 2;;
esac

if [ "$DRY" = 1 ]; then printf '%s\n' "$NEW"; exit 0; fi
[ "$(id -u)" = 0 ] || { echo "writing boot-args needs root: sudo $0 $CMD" >&2; exit 1; }
"$NVRAM" boot-args="$NEW" || { echo "nvram refused the write; nothing changed" >&2; exit 1; }
echo "boot-args now: $NEW"
echo "restart for the change to take effect; 'off' restores the default"
