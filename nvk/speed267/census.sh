#!/bin/bash
# census.sh <oldnakc> <newnakc> : every GB7 SPIR-V (native + translated), one line per kernel: name|old|new
cd ~/nvmtl-build/speed267 || exit 3
for f in gbspv/spv/*.spv xlspv/*.spv; do
  o=$(perl -e 'alarm 60; exec @ARGV' nakc/$1 $f main 2>&1 | tail -1)
  n=$(perl -e 'alarm 60; exec @ARGV' nakc/$2 $f main 2>&1 | tail -1)
  echo "$(basename $f)|$o|$n"
done
