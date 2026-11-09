#!/bin/bash
# The comparison tool, proven without a GPU: a dump made from curve_math.h passes, a dump of a curve with 1.5 times the radii fails.
set -eu
H=$(cd "$(dirname "$0")" && pwd); O=$(mktemp -d /tmp/oracle-self.XXXXXX)
cc -O2 -I"$H/.." -o "$O/gen" "$H/gen_dump.c" -lm; cc -O2 -I"$H/.." -o "$O/cmp" "$H/compare_oracle.c" -lm
"$O/gen" > "$O/same.txt"; "$O/gen" --perturb > "$O/other.txt"
rc=0; "$O/cmp" "$O/same.txt" > "$O/same.out" || rc=$?; tail -1 "$O/same.out"
if [ $rc -eq 0 ]; then echo "ok   a dump of the same curves passes"; else echo "FAIL the same curves did not pass"; exit 1; fi
rc=0; "$O/cmp" "$O/other.txt" > "$O/other.out" || rc=$?; tail -1 "$O/other.out"
if [ $rc -ne 0 ]; then echo "ok   control: a dump of fatter curves fails"; else echo "FAIL control: a dump of fatter curves passed"; exit 1; fi
rm -rf "$O"
